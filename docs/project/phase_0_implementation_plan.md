# ADK-Rust Implementation Plan

## Overview
This document outlines the phased implementation plan for converting ADK from Go to Rust. Each phase builds upon the previous, with clear milestones and deliverables.

## Phase 1: Foundation (Weeks 1-2)

### Milestone: Core traits and types established

#### Task 1.1: Project Setup
**Requirements**: TC-1, TC-3  
**Design**: Module Structure  
**Deliverables**:
- [ ] Create Cargo workspace with all crates
- [ ] Set up CI/CD (GitHub Actions)
- [ ] Configure linting (clippy, rustfmt)
- [ ] Add basic README and LICENSE
- [ ] Set up dependency versions

**Files to create**:
```
Cargo.toml (workspace)
adk-core/Cargo.toml
adk-agent/Cargo.toml
adk-model/Cargo.toml
adk-tool/Cargo.toml
adk-session/Cargo.toml
adk-artifact/Cargo.toml
adk-memory/Cargo.toml
adk-runner/Cargo.toml
```

#### Task 1.2: Core Error Types
**Requirements**: NFR-2.3, NFR-4.1  
**Design**: D-2 (Error Handling)  
**Deliverables**:
- [ ] Define `AdkError` enum with all error variants
- [ ] Implement `Result<T>` type alias
- [ ] Add error conversion traits
- [ ] Write error handling tests

**Files to create**:
```rust
adk-core/src/error.rs
adk-core/src/lib.rs
```

#### Task 1.3: Core Types and Traits
**Requirements**: FR-1.1, NFR-4.2  
**Design**: D-1 (Agent Trait), Module Structure  
**Deliverables**:
- [ ] Define `Agent` trait
- [ ] Define `Content`, `Part` types (from genai)
- [ ] Define `Event`, `EventActions` types
- [ ] Define context traits (`InvocationContext`, `CallbackContext`, etc.)
- [ ] Add comprehensive documentation

**Files to create**:
```rust
adk-core/src/agent.rs
adk-core/src/types.rs
adk-core/src/context.rs
adk-core/src/event.rs
```

#### Task 1.4: Model Trait
**Requirements**: FR-2.1, FR-2.3  
**Design**: D-1, Module Structure  
**Deliverables**:
- [ ] Define `Llm` trait
- [ ] Define `LlmRequest`, `LlmResponse` types
- [ ] Define streaming types
- [ ] Add mock implementation for testing

**Files to create**:
```rust
adk-core/src/model.rs
adk-model/src/lib.rs
adk-model/src/mock.rs
```

#### Task 1.5: Tool Trait
**Requirements**: FR-3.1, FR-3.4  
**Design**: D-5 (Tool System)  
**Deliverables**:
- [ ] Define `Tool` trait
- [ ] Define `ToolContext` type
- [ ] Define `Toolset` trait
- [ ] Add basic tool types

**Files to create**:
```rust
adk-core/src/tool.rs
adk-tool/src/lib.rs
```

## Phase 2: Session & Storage (Weeks 3-4)

### Milestone: Session management and storage working

#### Task 2.1: Session Types
**Requirements**: FR-4.1, FR-4.2, FR-4.3  
**Design**: D-6 (Session Storage)  
**Deliverables**:
- [ ] Define `Session` trait
- [ ] Define `State`, `Events` types
- [ ] Define `SessionService` trait
- [ ] Implement session validation

**Files to create**:
```rust
adk-session/src/lib.rs
adk-session/src/session.rs
adk-session/src/state.rs
adk-session/src/event.rs
adk-session/src/service.rs
```

#### Task 2.2: In-Memory Session Service
**Requirements**: FR-4.5  
**Design**: D-6  
**Deliverables**:
- [ ] Implement `InMemorySessionService`
- [ ] Add concurrent access with `Arc<RwLock<...>>`
- [ ] Write unit tests
- [ ] Add integration tests

**Files to create**:
```rust
adk-session/src/inmemory.rs
adk-session/tests/inmemory_tests.rs
```

#### Task 2.3: Database Session Service
**Requirements**: FR-4.5  
**Design**: D-6  
**Deliverables**:
- [ ] Implement `DatabaseSessionService` with SQLite
- [ ] Define database schema
- [ ] Add migrations
- [ ] Write tests with test database

**Files to create**:
```rust
adk-session/src/database.rs
adk-session/migrations/
adk-session/tests/database_tests.rs
```

#### Task 2.4: Artifact Service
**Requirements**: FR-5.1, FR-5.2, FR-5.3, FR-5.4  
**Design**: Module Structure  
**Deliverables**:
- [ ] Define `ArtifactService` trait
- [ ] Implement in-memory storage
- [ ] Implement GCS storage (optional for Phase 2)
- [ ] Add versioning support
- [ ] Write tests

**Files to create**:
```rust
adk-artifact/src/lib.rs
adk-artifact/src/service.rs
adk-artifact/src/inmemory.rs
adk-artifact/src/gcs.rs (optional)
adk-artifact/tests/artifact_tests.rs
```

#### Task 2.5: Memory Service
**Requirements**: FR-6.1, FR-6.2, FR-6.3  
**Design**: Module Structure  
**Deliverables**:
- [ ] Define `MemoryService` trait
- [ ] Implement basic in-memory implementation
- [ ] Add semantic search stub (simple keyword for now)
- [ ] Write tests

**Files to create**:
```rust
adk-memory/src/lib.rs
adk-memory/src/service.rs
adk-memory/src/inmemory.rs
adk-memory/tests/memory_tests.rs
```

## Phase 3: Model Integration (Weeks 5-6)

### Milestone: Gemini integration working with streaming

#### Task 3.1: Gemini Client
**Requirements**: FR-2.2, FR-2.3  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement Gemini API client
- [ ] Add authentication (API key)
- [ ] Implement request/response types
- [ ] Add error handling
- [ ] Write integration tests (with API key)

**Files to create**:
```rust
adk-model/src/gemini/mod.rs
adk-model/src/gemini/client.rs
adk-model/src/gemini/types.rs
adk-model/src/gemini/auth.rs
adk-model/tests/gemini_tests.rs
```

#### Task 3.2: Gemini Streaming
**Requirements**: FR-2.3  
**Design**: D-4 (Streaming Design)  
**Deliverables**:
- [ ] Implement streaming response handling
- [ ] Add stream aggregation
- [ ] Handle partial responses
- [ ] Write streaming tests

**Files to create**:
```rust
adk-model/src/gemini/streaming.rs
adk-model/tests/streaming_tests.rs
```

#### Task 3.3: Content Generation
**Requirements**: FR-2.4, FR-2.5, FR-2.6  
**Design**: D-1  
**Deliverables**:
- [ ] Implement `GenerateContent` method
- [ ] Support function calling
- [ ] Handle conversation history
- [ ] Add configuration options
- [ ] Write comprehensive tests

**Files to create**:
```rust
adk-model/src/gemini/generate.rs
adk-model/tests/generation_tests.rs
```

## Phase 4: Tool System (Weeks 7-8)

### Milestone: Tool execution and function tools working

#### Task 4.1: Function Tool
**Requirements**: FR-3.2  
**Design**: D-5 (Tool System)  
**Deliverables**:
- [ ] Implement `FunctionTool` with generics
- [ ] Add automatic schema inference
- [ ] Support async handlers
- [ ] Write tests with various function signatures

**Files to create**:
```rust
adk-tool/src/function_tool.rs
adk-tool/tests/function_tool_tests.rs
```

#### Task 4.2: Built-in Tools
**Requirements**: FR-3.3  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement Google Search tool
- [ ] Implement Exit Loop tool
- [ ] Add tool documentation
- [ ] Write tests

**Files to create**:
```rust
adk-tool/src/builtin/mod.rs
adk-tool/src/builtin/google_search.rs
adk-tool/src/builtin/exit_loop.rs
adk-tool/tests/builtin_tests.rs
```

#### Task 4.3: Toolset Implementation
**Requirements**: FR-3.5  
**Design**: D-5  
**Deliverables**:
- [ ] Implement `Toolset` trait
- [ ] Add dynamic tool filtering
- [ ] Support tool predicates
- [ ] Write tests

**Files to create**:
```rust
adk-tool/src/toolset.rs
adk-tool/tests/toolset_tests.rs
```

#### Task 4.4: Agent Tool
**Requirements**: FR-3.7  
**Design**: D-5  
**Deliverables**:
- [ ] Implement agent-as-tool wrapper
- [ ] Handle agent delegation
- [ ] Add proper context passing
- [ ] Write tests

**Files to create**:
```rust
adk-tool/src/agent_tool.rs
adk-tool/tests/agent_tool_tests.rs
```

## Phase 5: Agent Implementation (Weeks 9-11)

### Milestone: Core agents working (LLM, Custom, Workflow)

#### Task 5.1: Custom Agent
**Requirements**: FR-1.3  
**Design**: D-1, D-7 (Builder Pattern)  
**Deliverables**:
- [ ] Implement `CustomAgent` wrapper
- [ ] Add builder pattern
- [ ] Support callbacks
- [ ] Write tests

**Files to create**:
```rust
adk-agent/src/lib.rs
adk-agent/src/custom_agent.rs
adk-agent/tests/custom_agent_tests.rs
```

#### Task 5.2: LLM Agent Core
**Requirements**: FR-1.2, FR-2.5  
**Design**: D-1, D-7  
**Deliverables**:
- [ ] Implement `LlmAgent` struct
- [ ] Add builder pattern
- [ ] Implement basic run loop
- [ ] Handle model interaction
- [ ] Write tests with mock model

**Files to create**:
```rust
adk-agent/src/llm_agent.rs
adk-agent/tests/llm_agent_tests.rs
```

#### Task 5.3: LLM Agent Tool Execution
**Requirements**: FR-2.5, FR-3.1  
**Design**: D-5  
**Deliverables**:
- [ ] Implement function call processing
- [ ] Add parallel tool execution
- [ ] Handle tool responses
- [ ] Add error handling
- [ ] Write tests

**Files to create**:
```rust
adk-agent/src/llm_agent/tool_execution.rs
adk-agent/tests/tool_execution_tests.rs
```

#### Task 5.4: LLM Agent State Management
**Requirements**: FR-4.3, FR-4.4  
**Design**: D-3 (Context Management)  
**Deliverables**:
- [ ] Implement state updates
- [ ] Handle state scoping
- [ ] Add state delta tracking
- [ ] Write tests

**Files to create**:
```rust
adk-agent/src/llm_agent/state.rs
adk-agent/tests/state_tests.rs
```

#### Task 5.5: Sequential Agent
**Requirements**: FR-1.6  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement `SequentialAgent`
- [ ] Add sub-agent execution
- [ ] Handle event aggregation
- [ ] Write tests

**Files to create**:
```rust
adk-agent/src/workflow/mod.rs
adk-agent/src/workflow/sequential.rs
adk-agent/tests/sequential_tests.rs
```

#### Task 5.6: Parallel Agent
**Requirements**: FR-1.6  
**Design**: Module Structure, Concurrency Model  
**Deliverables**:
- [ ] Implement `ParallelAgent`
- [ ] Add concurrent execution with `tokio::spawn`
- [ ] Aggregate results
- [ ] Write tests

**Files to create**:
```rust
adk-agent/src/workflow/parallel.rs
adk-agent/tests/parallel_tests.rs
```

#### Task 5.7: Loop Agent
**Requirements**: FR-1.6  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement `LoopAgent`
- [ ] Add iteration control
- [ ] Support exit conditions
- [ ] Write tests

**Files to create**:
```rust
adk-agent/src/workflow/loop_agent.rs
adk-agent/tests/loop_tests.rs
```

## Phase 6: Runner & Execution (Weeks 12-13)

### Milestone: End-to-end agent execution working

#### Task 6.1: Invocation Context
**Requirements**: FR-7.2  
**Design**: D-3 (Context Management)  
**Deliverables**:
- [ ] Implement `InvocationContext`
- [ ] Add service access methods
- [ ] Implement context builders
- [ ] Write tests

**Files to create**:
```rust
adk-runner/src/lib.rs
adk-runner/src/context.rs
adk-runner/tests/context_tests.rs
```

#### Task 6.2: Callback System
**Requirements**: FR-7.4  
**Design**: D-8 (Callback System)  
**Deliverables**:
- [ ] Implement callback types
- [ ] Add callback execution
- [ ] Handle callback errors
- [ ] Write tests

**Files to create**:
```rust
adk-runner/src/callbacks.rs
adk-runner/tests/callback_tests.rs
```

#### Task 6.3: Runner Core
**Requirements**: FR-7.1, FR-7.3, FR-7.5  
**Design**: Data Flow  
**Deliverables**:
- [ ] Implement `Runner` struct
- [ ] Add agent tree traversal
- [ ] Implement agent selection logic
- [ ] Handle event streaming
- [ ] Write integration tests

**Files to create**:
```rust
adk-runner/src/runner.rs
adk-runner/tests/runner_tests.rs
```

#### Task 6.4: Agent Transfer
**Requirements**: FR-1.5  
**Design**: Data Flow  
**Deliverables**:
- [ ] Implement agent transfer logic
- [ ] Handle parent/peer transfers
- [ ] Add transfer validation
- [ ] Write tests

**Files to create**:
```rust
adk-runner/src/transfer.rs
adk-runner/tests/transfer_tests.rs
```

## Phase 7: Server & API (Weeks 14-16)

### Milestone: REST and A2A servers operational

#### Task 7.1: REST API Foundation
**Requirements**: FR-8.1  
**Design**: Module Structure  
**Deliverables**:
- [ ] Set up Axum/Actix-web server
- [ ] Define route structure
- [ ] Add middleware (logging, CORS)
- [ ] Write basic tests

**Files to create**:
```rust
adk-server/Cargo.toml
adk-server/src/lib.rs
adk-server/src/rest/mod.rs
adk-server/src/rest/routes.rs
```

#### Task 7.2: Session Endpoints
**Requirements**: FR-8.3  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement session CRUD endpoints
- [ ] Add request/response types
- [ ] Handle errors properly
- [ ] Write API tests

**Files to create**:
```rust
adk-server/src/rest/controllers/session.rs
adk-server/tests/session_api_tests.rs
```

#### Task 7.3: Runtime Endpoints
**Requirements**: FR-8.4  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement agent execution endpoint
- [ ] Add streaming support (SSE)
- [ ] Handle cancellation
- [ ] Write API tests

**Files to create**:
```rust
adk-server/src/rest/controllers/runtime.rs
adk-server/tests/runtime_api_tests.rs
```

#### Task 7.4: Artifact Endpoints
**Requirements**: FR-8.5  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement artifact CRUD endpoints
- [ ] Handle file uploads/downloads
- [ ] Add versioning support
- [ ] Write API tests

**Files to create**:
```rust
adk-server/src/rest/controllers/artifact.rs
adk-server/tests/artifact_api_tests.rs
```

#### Task 7.5: A2A Protocol
**Requirements**: FR-8.2  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement A2A protocol types
- [ ] Add agent card generation
- [ ] Implement A2A endpoints
- [ ] Write protocol tests

**Files to create**:
```rust
adk-server/src/a2a/mod.rs
adk-server/src/a2a/protocol.rs
adk-server/src/a2a/agent_card.rs
adk-server/tests/a2a_tests.rs
```

## Phase 8: CLI & Examples (Weeks 17-18)

### Milestone: CLI working with examples

#### Task 8.1: CLI Foundation
**Requirements**: FR-9.1  
**Design**: Module Structure  
**Deliverables**:
- [ ] Set up CLI with clap
- [ ] Define command structure
- [ ] Add configuration loading
- [ ] Write CLI tests

**Files to create**:
```rust
adk-cli/Cargo.toml
adk-cli/src/main.rs
adk-cli/src/cli.rs
adk-cli/src/config.rs
```

#### Task 8.2: Console Mode
**Requirements**: FR-9.2  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement interactive console
- [ ] Add readline support
- [ ] Handle streaming output
- [ ] Add history

**Files to create**:
```rust
adk-cli/src/console.rs
```

#### Task 8.3: Launcher
**Requirements**: FR-9.3, FR-9.4  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement launcher modes
- [ ] Add server startup
- [ ] Support different configurations
- [ ] Write tests

**Files to create**:
```rust
adk-cli/src/launcher.rs
```

#### Task 8.4: Quickstart Example
**Requirements**: NFR-6.4  
**Design**: Module Structure  
**Deliverables**:
- [ ] Port quickstart example
- [ ] Add documentation
- [ ] Ensure it runs correctly

**Files to create**:
```rust
examples/quickstart.rs
examples/README.md
```

#### Task 8.5: Tool Examples
**Requirements**: NFR-6.4  
**Design**: Module Structure  
**Deliverables**:
- [ ] Create function tool example
- [ ] Create multiple tools example
- [ ] Add documentation

**Files to create**:
```rust
examples/tools/function_tool.rs
examples/tools/multiple_tools.rs
```

#### Task 8.6: Workflow Examples
**Requirements**: NFR-6.4  
**Design**: Module Structure  
**Deliverables**:
- [ ] Create sequential agent example
- [ ] Create parallel agent example
- [ ] Create loop agent example
- [ ] Add documentation

**Files to create**:
```rust
examples/workflow/sequential.rs
examples/workflow/parallel.rs
examples/workflow/loop.rs
```

## Phase 9: Advanced Features (Weeks 19-20)

### Milestone: MCP, Remote Agent, Advanced tools

#### Task 9.1: MCP Integration
**Requirements**: FR-3.6  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement MCP protocol client
- [ ] Add MCP toolset
- [ ] Support MCP servers
- [ ] Write tests

**Files to create**:
```rust
adk-tool/src/mcp/mod.rs
adk-tool/src/mcp/client.rs
adk-tool/src/mcp/toolset.rs
adk-tool/tests/mcp_tests.rs
```

#### Task 9.2: Remote Agent (A2A Client)
**Requirements**: FR-8.2  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement A2A client
- [ ] Add remote agent wrapper
- [ ] Handle network errors
- [ ] Write tests

**Files to create**:
```rust
adk-agent/src/remote_agent.rs
adk-agent/tests/remote_agent_tests.rs
```

#### Task 9.3: Load Artifacts Tool
**Requirements**: FR-5.1  
**Design**: Module Structure  
**Deliverables**:
- [ ] Implement load artifacts tool
- [ ] Add artifact listing
- [ ] Support versioning
- [ ] Write tests

**Files to create**:
```rust
adk-tool/src/builtin/load_artifacts.rs
```

## Phase 10: Polish & Documentation (Weeks 21-22)

### Milestone: Production-ready release

#### Task 10.1: Documentation
**Requirements**: NFR-7.1, NFR-7.2, NFR-7.3, NFR-7.4  
**Deliverables**:
- [ ] Complete all rustdoc comments
- [ ] Write architecture guide
- [ ] Create migration guide from Go
- [ ] Add usage tutorials
- [ ] Generate docs with `cargo doc`

**Files to create**:
```
docs/architecture.md
docs/migration-from-go.md
docs/tutorials/
```

#### Task 10.2: Testing & Coverage
**Requirements**: NFR-6.1, NFR-6.2  
**Deliverables**:
- [ ] Achieve >80% code coverage
- [ ] Add missing unit tests
- [ ] Add integration tests
- [ ] Add benchmark tests

#### Task 10.3: Performance Optimization
**Requirements**: NFR-1.1, NFR-1.2  
**Deliverables**:
- [ ] Profile hot paths
- [ ] Optimize allocations
- [ ] Reduce cloning
- [ ] Benchmark improvements

#### Task 10.4: Security Audit/compact
**Requirements**: NFR-2.1, NFR-2.2  
**Deliverables**:
- [ ] Run `cargo audit`
- [ ] Review unsafe code
- [ ] Add input validation
- [ ] Test error paths

#### Task 10.5: Deployment
**Requirements**: NFR-8.1, NFR-8.2, NFR-8.3  
**Deliverables**:
- [ ] Create Dockerfile
- [ ] Add Docker Compose example
- [ ] Document cloud deployment
- [ ] Test cross-compilation

**Files to create**:
```
Dockerfile
docker-compose.yml
deploy/
```

#### Task 10.6: Release Preparation
**Requirements**: All  
**Deliverables**:
- [ ] Version all crates (0.1.0)
- [ ] Write CHANGELOG
- [ ] Create release notes
- [ ] Publish to crates.io (optional)

## Success Criteria

### Phase Completion
- All tasks in phase completed
- All tests passing
- Documentation updated
- Code reviewed

### Final Release
- All functional requirements met
- All non-functional requirements met
- Examples working
- Documentation complete
- CI/CD passing
- Security audit passed

## Risk Mitigation

### Technical Risks
- **Async complexity**: Start simple, add complexity incrementally
- **Gemini API changes**: Abstract behind trait, version carefully
- **Performance issues**: Profile early, optimize incrementally

### Schedule Risks
- **Underestimation**: Add 20% buffer to each phase
- **Blocking issues**: Identify dependencies early, parallelize where possible
- **Scope creep**: Defer non-essential features to post-1.0

## Dependencies Between Tasks

```
1.1 → 1.2 → 1.3 → 1.4 → 1.5
       ↓     ↓     ↓     ↓
      2.1 → 2.2 → 2.3   2.4, 2.5
             ↓           ↓
           3.1 → 3.2 → 3.3
                   ↓
           4.1 → 4.2 → 4.3 → 4.4
                         ↓
           5.1 → 5.2 → 5.3 → 5.4
                   ↓
           5.5, 5.6, 5.7
                   ↓
           6.1 → 6.2 → 6.3 → 6.4
                         ↓
           7.1 → 7.2, 7.3, 7.4, 7.5
                         ↓
           8.1 → 8.2, 8.3 → 8.4, 8.5, 8.6
                         ↓
           9.1, 9.2, 9.3
                   ↓
           10.1 → 10.2 → 10.3 → 10.4 → 10.5 → 10.6
```

## Estimated Timeline

- **Phase 1**: 2 weeks
- **Phase 2**: 2 weeks
- **Phase 3**: 2 weeks
- **Phase 4**: 2 weeks
- **Phase 5**: 3 weeks
- **Phase 6**: 2 weeks
- **Phase 7**: 3 weeks
- **Phase 8**: 2 weeks
- **Phase 9**: 2 weeks
- **Phase 10**: 2 weeks

**Total**: 22 weeks (~5.5 months)

With buffer: **26 weeks (~6.5 months)**
