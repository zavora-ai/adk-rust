# ADK Rust Examples

This directory contains example applications demonstrating how to use the ADK Rust framework.

## Structure

Each example is in its own directory with a `main.rs` file:

```
examples/
├── quickstart/          # Simple weather agent
├── function_tool/       # Custom function tool
├── multiple_tools/      # Agent composition
├── server/              # REST API server
├── a2a/                 # A2A protocol
├── web/                 # Multi-agent server
├── sequential/          # Sequential workflow
├── sequential_code/     # Code generation workflow
├── parallel/            # Parallel workflow
├── loop_workflow/       # Iterative loop
├── load_artifacts/      # Artifact loading
├── mcp/                 # MCP integration
└── research_paper/      # Full-stack research paper generator
```

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

### Server Examples

#### server
Starts an HTTP server with REST and A2A endpoints:
```bash
cargo run --example server
# or with custom port
PORT=3000 cargo run --example server
```
Demonstrates: Server mode, agent loader, HTTP endpoints.

#### a2a
Agent-to-Agent protocol demonstration:
```bash
cargo run --example a2a
```
Demonstrates: A2A agent card generation, protocol integration pattern.

#### web
Multi-agent web application with artifact support:
```bash
cargo run --example web
```
Demonstrates: Multiple specialized agents, MultiAgentLoader, REST server with agent selection.

### Workflow Examples

#### sequential
Sequential workflow processing (analyze → expand → summarize):
```bash
cargo run --example sequential
```
Demonstrates: Sequential agent execution, multi-step processing.

#### sequential_code
Code generation workflow (design → implement → review):
```bash
cargo run --example sequential_code
```
Demonstrates: Sequential workflow for code generation, multi-stage refinement.

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

### Tool Examples

#### load_artifacts
Demonstrate artifact loading and management:
```bash
cargo run --example load_artifacts
```
Demonstrates: LoadArtifactsTool, artifact service integration.

#### mcp
Model Context Protocol integration:
```bash
cargo run --example mcp
```
Demonstrates: McpToolset integration pattern.

### Full-Stack Examples

#### research_paper
Complete client-server application for generating research papers:
```bash
cargo run --example research_paper -- serve --port 8080
```
Then open `examples/research_paper/frontend.html` in your browser.

Demonstrates: 
- Full-stack architecture (frontend + backend)
- Custom research and PDF generation tools
- Real-time SSE streaming to web client
- Artifact storage and download
- Session management
- Production-ready integration patterns

See [research_paper/README.md](research_paper/README.md) for detailed documentation.

## Example Categories

| Category | Count | Examples |
|----------|-------|----------|
| **Basic** | 3 | quickstart, function_tool, multiple_tools |
| **Servers** | 3 | server, a2a, web |
| **Workflows** | 4 | sequential, sequential_code, parallel, loop_workflow |
| **Tools** | 2 | load_artifacts, mcp |
| **Full-Stack** | 1 | research_paper |
| **Total** | **13** | |

## Parity with Go ADK

| Go Example | Rust Example | Status |
|------------|--------------|--------|
| quickstart | quickstart | ✅ Complete |
| rest | server | ✅ Complete |
| a2a | a2a | ✅ Complete |
| mcp | mcp | ✅ Complete |
| web | web | ✅ Complete |
| tools/multipletools | multiple_tools | ✅ Complete |
| tools/loadartifacts | load_artifacts | ✅ Complete |
| workflowagents/sequential | sequential | ✅ Complete |
| workflowagents/sequentialCode | sequential_code | ✅ Complete |
| workflowagents/parallel | parallel | ✅ Complete |
| workflowagents/loop | loop_workflow | ✅ Complete |
| vertexai/imagegenerator | - | ⏸️ Deferred (requires Vertex AI) |

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
- MCP and A2A examples show integration patterns (placeholders)
