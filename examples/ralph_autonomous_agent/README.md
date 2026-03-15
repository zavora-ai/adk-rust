# Ralph Autonomous Agent

A fully native ADK-Rust autonomous agent that continuously executes development tasks from a Product Requirements Document (PRD) until all items are complete.

## Overview

This example demonstrates a production-ready autonomous agent built with ADK-Rust that:

- Uses a two-agent architecture (Loop Agent + Worker Agent)
- Manages tasks through a structured PRD (Product Requirements Document)
- Enforces quality gates with cargo check/test/clippy
- Supports multiple LLM providers (OpenAI, Anthropic, Gemini)
- Provides comprehensive logging and progress tracking

## Architecture

Ralph follows a clean separation of concerns:

- **Loop Agent**: Orchestrates the workflow, manages task iteration and delegation
- **Worker Agent**: Executes individual tasks with quality gate enforcement
- **Tool System**: Specialized tools for different operations (PRD, Git, File, Test)
- **Model Integration**: Flexible LLM provider support with environment-based configuration

## Running the Example

1. Set up your environment:
```bash
cd examples/ralph_autonomous_agent
cp .env.example .env
# Edit .env with your API keys and configuration
```

2. Run the example:
```bash
cargo run --example ralph_autonomous_agent
```

## Configuration

Configure Ralph through environment variables in `.env`:

| Variable | Description | Default |
|----------|-------------|---------|
| `RALPH_MODEL_PROVIDER` | Model provider (openai, anthropic, gemini) | openai |
| `RALPH_MODEL_NAME` | Specific model name | gpt-4 |
| `RALPH_MAX_ITERATIONS` | Maximum iterations before terminating | 50 |
| `RALPH_PRD_PATH` | Path to PRD JSON file | prd.json |
| `RALPH_PROGRESS_PATH` | Path to progress log file | progress.md |
| `RALPH_AGENTS_MD_PATH` | Path to AGENTS.md file | AGENTS.md |
| `RALPH_PROJECT_PATH` | Base project directory | . |

## Current Status

This example currently demonstrates the basic project structure setup. Future tasks will implement:

- PRD loading and management
- Tool system (Git, File, Test, PRD tools)
- Loop Agent and Worker Agent implementations
- Model integration for all providers
- Quality gate enforcement
- Complete autonomous workflow

## Implementation Plan

The full implementation follows the spec-driven development approach with:

1. **Core Data Models**: PRD structures with serialization
2. **Tool System**: Specialized tools for different operations
3. **Agent System**: Loop and Worker agents with proper delegation
4. **Model Integration**: Multi-provider support with factory pattern
5. **Quality Assurance**: Automated checks before commits
6. **Error Handling**: Comprehensive error types with actionable messages
7. **Logging**: Structured progress tracking and debugging

## Next Steps

To complete the implementation, execute the remaining tasks from the specification:

1. Implement core data models (Task 2)
2. Create the tool system (Task 3)
3. Build the agent system (Task 5)
4. Add model integration (Task 6)
5. Implement error handling and logging (Task 7)
6. Create the main application entry point (Task 8)

Each task builds incrementally on the previous ones, following ADK-Rust best practices and patterns.