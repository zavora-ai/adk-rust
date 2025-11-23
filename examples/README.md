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

### Quickstart

A simple weather and time agent using Google Search:

```bash
cargo run --example quickstart
```

This example demonstrates:
- Creating a Gemini model
- Building an LLM agent with tools
- Running an interactive console session

## Example Structure

Each example is a standalone Rust file in this directory that can be run with:
```bash
cargo run --example <example_name>
```

## Available Examples

- **quickstart** - Basic agent with Google Search tool

## More Examples Coming Soon

- Function tools
- Multiple tools
- Workflow agents (sequential, parallel, loop)
