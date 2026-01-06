# adk-studio

Visual development environment for AI agents built with Rust Agent Development Kit (ADK-Rust).

[![Crates.io](https://img.shields.io/crates/v/adk-studio.svg)](https://crates.io/crates/adk-studio)
[![Documentation](https://docs.rs/adk-studio/badge.svg)](https://docs.rs/adk-studio)
[![License](https://img.shields.io/crates/l/adk-studio.svg)](LICENSE)

![ADK Studio - Support Router Template](docs/studio-screenshot.png)

## Overview

`adk-studio` provides a visual, low-code development environment for building AI agents with [ADK-Rust](https://github.com/zavora-ai/adk-rust):

- **Drag-and-Drop Canvas** - Visual workflow design with ReactFlow
- **Agent Palette** - LLM Agent, Sequential, Parallel, Loop, Router agents
- **Tool Integration** - Function, MCP, Browser, Google Search tools
- **Real-Time Chat** - Test agents with live SSE streaming
- **Code Generation** - Compile visual designs to production Rust code
- **Build System** - Compile and run executables directly from Studio

## Installation

```bash
cargo install adk-studio
```

Or build from source:

```bash
cargo build --release -p adk-studio
```

## Quick Start

```bash
# Start ADK Studio server
adk-studio

# Open in browser
open http://localhost:3000
```

### With Custom Host

```bash
# Bind to all interfaces (for remote access)
adk-studio --host 0.0.0.0 --port 8080
```

## Features

### Visual Agent Builder
- Drag agents from palette onto canvas
- Connect agents to create workflows (Sequential, Parallel, Loop)
- Configure agent properties: name, model, instructions, tools
- Add sub-agents to container nodes

### Tool Support
- **Function Tools** - Custom functions with code editor and templates
- **MCP Tools** - Model Context Protocol servers with templates
- **Browser Tools** - Web automation with 46 WebDriver actions
- **Google Search** - Grounded search queries
- **Load Artifact** - Load binary artifacts into context

### Real-Time Execution
- Live SSE streaming with agent animations
- Event trace panel for debugging
- Session memory persistence
- Thought bubble visualization

### Code Generation
- **View Code** - See generated Rust code with Monaco Editor
- **Compile** - Generate Rust project from visual design
- **Build** - Compile to executable with real-time output
- **Run** - Execute built agent directly

## Architecture

```
adk-studio/
├── src/
│   ├── main.rs        # CLI entry point
│   ├── server.rs      # Axum HTTP server
│   ├── routes/        # API endpoints
│   ├── codegen/       # Rust code generation
│   └── templates/     # Agent templates
└── studio-ui/         # React frontend (separate package)
```

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/projects` | GET/POST | List/create projects |
| `/api/projects/:id` | GET/PUT/DELETE | Project CRUD |
| `/api/projects/:id/codegen` | POST | Generate Rust code |
| `/api/projects/:id/build` | POST | Compile project |
| `/api/projects/:id/run` | POST | Run built executable |
| `/api/chat` | POST | Send chat message (SSE stream) |

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `GEMINI_API_KEY` | Google Gemini API key | Required |
| `ADK_DEV_MODE` | Use local workspace dependencies | `false` |
| `RUST_LOG` | Log level | `info` |

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations
- [adk-graph](https://crates.io/crates/adk-graph) - LangGraph-style workflows
- [adk-tool](https://crates.io/crates/adk-tool) - Tool system

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
