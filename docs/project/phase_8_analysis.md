# Phase 8: CLI & Examples - Analysis

## Overview
Phase 8 focuses on building the CLI/launcher system and example applications. This analysis compares the Go implementation with our requirements and current Rust implementation state.

## Requirements Review

### FR-9: CLI/Launcher Requirements
- **FR-9.1**: Provide command-line interface for agent execution
- **FR-9.2**: Support console mode for interactive chat
- **FR-9.3**: Support web UI launcher
- **FR-9.4**: Enable production deployment mode

### NFR-6.4: Examples
- Provide working examples demonstrating key features
- Include quickstart, tools, and workflow examples

## Go Implementation Architecture

### 1. Launcher System (`cmd/launcher/`)

#### Core Interfaces
```go
// Launcher - main interface for running ADK applications
type Launcher interface {
    Execute(ctx context.Context, config *Config, args []string) error
    CommandLineSyntax() string
}

// SubLauncher - composable launcher for specific modes
type SubLauncher interface {
    Keyword() string
    Parse(args []string) ([]string, error)
    CommandLineSyntax() string
    SimpleDescription() string
    Run(ctx context.Context, config *Config) error
}

// Config - shared configuration
type Config struct {
    SessionService  session.Service
    ArtifactService artifact.Service
    MemoryService   memory.Service
    AgentLoader     agent.Loader
    A2AOptions      []a2asrv.RequestHandlerOption
}
```

#### Key Components

**1. Universal Launcher** (`universal/universal.go`)
- Routes to sublaunchers based on command-line keyword
- First sublauncher is default if no keyword specified
- Parses args and delegates to chosen sublauncher
- ~150 lines

**2. Console Launcher** (`console/console.go`)
- Interactive REPL for agent interaction
- Supports streaming modes (SSE/None)
- Uses bufio.Reader for input
- Creates session and runner
- Prints agent responses in real-time
- ~180 lines

**3. Web Launcher** (`web/web.go`)
- HTTP server with configurable port and timeouts
- Supports multiple sublaunchers (API, A2A, WebUI)
- Uses gorilla/mux for routing
- Each sublauncher adds its own routes
- ~240 lines

**4. Full Launcher** (`full/full.go`)
- Combines console + web (API + A2A + WebUI)
- Production-ready with all features
- ~30 lines (composition)

**5. Prod Launcher** (`prod/prod.go`)
- Web-only (API + A2A, no console/WebUI)
- For production deployments
- ~30 lines (composition)

### 2. CLI Tool (`cmd/adkgo/`)

**adkgo** - Deployment and testing CLI
- Uses cobra for command structure
- Supports Cloud Run deployment
- ~25 lines main + internal commands

### 3. Examples

#### Quickstart (`examples/quickstart/main.go`)
- Creates Gemini model with API key
- Builds LLM agent with GoogleSearch tool
- Uses full launcher for console/web modes
- ~60 lines

#### Multiple Tools (`examples/tools/multipletools/main.go`)
- Demonstrates workaround for multiple tool types
- Creates sub-agents for different tool types
- Root agent orchestrates via AgentTool
- ~100 lines

#### Workflow Examples
- **Sequential**: Chain of agents
- **Parallel**: Concurrent agent execution
- **Loop**: Iterative agent processing

## Current Rust Implementation Status

### Completed (Phases 1-7)
✅ Core traits and types (adk-core)
✅ Session management (adk-session)
✅ Artifact storage (adk-artifact)
✅ Memory system (adk-memory)
✅ Model integration (adk-model with gemini-rust)
✅ Tool system (adk-tool)
✅ Agent implementations (adk-agent)
✅ Runner & execution (adk-runner)
✅ Server & API (adk-server with REST + A2A)

### Missing (Phase 8)
❌ CLI crate (adk-cli)
❌ Launcher system
❌ Console mode
❌ Examples directory (empty)

## Implementation Plan for Phase 8

### Task 8.1: CLI Foundation
**Goal**: Create adk-cli crate with clap-based CLI structure

**Files to create**:
```
adk-cli/
├── Cargo.toml
└── src/
    ├── main.rs          # Entry point
    ├── cli.rs           # Clap command definitions
    ├── config.rs        # Configuration loading
    └── launcher/
        └── mod.rs       # Launcher trait
```

**Key decisions**:
- Use `clap` v4 with derive macros
- Support subcommands: `console`, `web`, `serve`
- Load config from env vars and optional config file
- Minimal ~200 lines total

### Task 8.2: Console Mode
**Goal**: Interactive REPL for agent interaction

**Files to create**:
```
adk-cli/src/
└── launcher/
    └── console.rs       # Console launcher
```

**Features**:
- Use `rustyline` for readline support with history
- Stream events from Runner
- Print responses in real-time
- Support Ctrl+C gracefully
- ~150 lines

**Key differences from Go**:
- Rust: `rustyline` vs Go: `bufio.Reader`
- Rust: Tokio async vs Go: goroutines
- Rust: Stream trait vs Go: range over channel

### Task 8.3: Web/Server Launcher
**Goal**: Launch adk-server with configuration

**Files to create**:
```
adk-cli/src/
└── launcher/
    ├── web.rs           # Web launcher
    └── serve.rs         # Production server launcher
```

**Features**:
- Parse port, timeouts from CLI
- Build ServerConfig from CLI args
- Start Axum server
- Support graceful shutdown
- ~100 lines each

**Integration**:
- Reuse existing adk-server implementation
- No need for sublauncher pattern (simpler in Rust)

### Task 8.4: Quickstart Example
**Goal**: Port quickstart example

**Files to create**:
```
examples/
├── Cargo.toml           # Workspace member
└── quickstart.rs        # Main example
```

**Content**:
- Create Gemini model
- Build LLM agent with GoogleSearch
- Use console launcher
- ~50 lines

### Task 8.5: Tool Examples
**Goal**: Demonstrate tool usage

**Files to create**:
```
examples/
├── function_tool.rs     # Custom function tool
└── multiple_tools.rs    # Multiple tool types
```

**Content**:
- function_tool: Simple calculator tool
- multiple_tools: Sub-agent pattern for tool mixing
- ~80 lines each

### Task 8.6: Workflow Examples
**Goal**: Demonstrate workflow agents

**Files to create**:
```
examples/
├── sequential.rs        # Sequential workflow
├── parallel.rs          # Parallel workflow
└── loop.rs              # Loop workflow
```

**Content**:
- Use existing workflow agent implementations
- Show real-world use cases
- ~60 lines each

## Key Design Decisions

### 1. Simplified Launcher Pattern
**Go approach**: Complex SubLauncher interface with keyword routing
**Rust approach**: Simple clap subcommands

**Rationale**:
- Clap provides better UX than custom parsing
- No need for dynamic sublauncher composition
- Simpler to maintain and extend

### 2. No Separate adkgo Tool
**Go**: Separate `adkgo` CLI for deployment
**Rust**: Single `adk` CLI with subcommands

**Rationale**:
- Unified CLI is more intuitive
- Can add `deploy` subcommand later if needed
- Focus on core functionality first

### 3. Examples as Workspace Members
**Structure**:
```toml
[workspace]
members = [
    "adk-core",
    "adk-agent",
    # ... other crates
    "examples",
]
```

**Rationale**:
- Examples can depend on all adk-* crates
- Single `cargo run --example quickstart`
- Easier to maintain and test

### 4. Rustyline for Console
**Choice**: `rustyline` crate for REPL
**Features**:
- Line editing (Emacs/Vi modes)
- History support
- Tab completion (future)
- Cross-platform

## Dependencies

### New Dependencies for adk-cli
```toml
[dependencies]
clap = { version = "4.5", features = ["derive"] }
rustyline = "14.0"
tokio = { version = "1.40", features = ["full"] }
adk-core = { path = "../adk-core" }
adk-agent = { path = "../adk-agent" }
adk-model = { path = "../adk-model" }
adk-runner = { path = "../adk-runner" }
adk-server = { path = "../adk-server" }
adk-session = { path = "../adk-session" }
anyhow = "1.0"
```

### Examples Dependencies
```toml
[dependencies]
adk-core = { path = "../adk-core" }
adk-agent = { path = "../adk-agent" }
adk-model = { path = "../adk-model" }
adk-tool = { path = "../adk-tool" }
adk-runner = { path = "../adk-runner" }
adk-session = { path = "../adk-session" }
tokio = { version = "1.40", features = ["full"] }
anyhow = "1.0"
```

## Implementation Estimates

### Task 8.1: CLI Foundation
- **Effort**: 2-3 hours
- **Lines**: ~200
- **Complexity**: Low (clap is straightforward)

### Task 8.2: Console Mode
- **Effort**: 3-4 hours
- **Lines**: ~150
- **Complexity**: Medium (async streaming, rustyline integration)

### Task 8.3: Web/Server Launcher
- **Effort**: 2-3 hours
- **Lines**: ~200
- **Complexity**: Low (reuse adk-server)

### Task 8.4: Quickstart Example
- **Effort**: 1 hour
- **Lines**: ~50
- **Complexity**: Low (straightforward port)

### Task 8.5: Tool Examples
- **Effort**: 2-3 hours
- **Lines**: ~160
- **Complexity**: Medium (sub-agent pattern)

### Task 8.6: Workflow Examples
- **Effort**: 2-3 hours
- **Lines**: ~180
- **Complexity**: Low (use existing agents)

**Total Estimate**: 12-19 hours, ~940 lines

## Testing Strategy

### CLI Tests
- Unit tests for config parsing
- Integration tests for launcher execution
- Mock agents for testing

### Example Tests
- Each example should run without errors
- Use `cargo test --examples`
- Verify output format

### Manual Testing
- Run console mode interactively
- Test web launcher with curl
- Verify all examples work end-to-end

## Success Criteria

### Functional
✅ CLI runs with `cargo run --bin adk`
✅ Console mode provides interactive REPL
✅ Web launcher starts server on specified port
✅ All examples run successfully
✅ Examples demonstrate key features

### Quality
✅ All tests passing
✅ Documentation for CLI usage
✅ README for examples
✅ No clippy warnings

### User Experience
✅ Intuitive command structure
✅ Helpful error messages
✅ Good readline experience (history, editing)
✅ Clear example output

## Next Steps

1. **Create adk-cli crate** with basic structure
2. **Implement console launcher** with rustyline
3. **Implement web launcher** reusing adk-server
4. **Port quickstart example** as proof of concept
5. **Add tool examples** demonstrating features
6. **Add workflow examples** showing composition
7. **Write documentation** for CLI and examples
8. **Test end-to-end** with real agents

## Notes

- Phase 8 is lighter than Phase 7 (no protocol implementation)
- Most complexity is in Phase 2 console mode (async + rustyline)
- Examples are straightforward ports from Go
- Can defer web UI (webui sublauncher) to later phase
- Focus on core CLI and essential examples first
