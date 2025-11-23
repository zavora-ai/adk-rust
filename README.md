# ADK-Rust: Agent Development Kit in Rust

A Rust implementation of Google's Agent Development Kit (ADK), providing a high-performance, memory-safe framework for building AI agents.

## 📋 Project Status

**Phase**: Planning Complete  
**Implementation**: Not Started  
**Target Release**: 6.5 months from start

## 🎯 Project Overview

This project converts the [Google ADK for Go](https://github.com/google/adk-go) to Rust, maintaining feature parity while leveraging Rust's strengths in:
- **Memory Safety**: Zero-cost abstractions without garbage collection
- **Concurrency**: Safe concurrent execution with Tokio
- **Performance**: Efficient async I/O and minimal allocations
- **Type Safety**: Compile-time guarantees for correctness

## 📚 Documentation

### Planning Documents
- **[REQUIREMENTS.md](REQUIREMENTS.md)** - Comprehensive functional and non-functional requirements (83 total)
- **[DESIGN.md](DESIGN.md)** - Architecture, design decisions, and technical approach
- **[IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md)** - Phased implementation with 60+ tasks over 10 phases
- **[PROJECT_SUMMARY.md](PROJECT_SUMMARY.md)** - Executive summary and overview

### Quick Links
- [Source Repository (Go)](https://github.com/google/adk-go) - Original implementation
- [Requirements Overview](#requirements-overview)
- [Architecture Overview](#architecture-overview)
- [Implementation Timeline](#implementation-timeline)

## 🏗️ Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    Application Layer                         │
│         (CLI, REST Server, A2A Server, Examples)            │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                       Runner Layer                           │
│    (Agent Execution, Context Management, Event Streaming)    │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                       Agent Layer                            │
│   (Agent Trait, LLMAgent, WorkflowAgents, CustomAgent)      │
└─────────────────────────────────────────────────────────────┘
                              │
┌──────────────┬──────────────┬──────────────┬────────────────┐
│   Model      │    Tool      │   Session    │   Services     │
│   Layer      │    Layer     │   Layer      │   Layer        │
└──────────────┴──────────────┴──────────────┴────────────────┘
```

### Module Structure

```
adk-rust/
├── adk-core/          # Core traits and types
├── adk-agent/         # Agent implementations
├── adk-model/         # Model implementations (Gemini)
├── adk-tool/          # Tool implementations
├── adk-session/       # Session management
├── adk-artifact/      # Artifact storage
├── adk-memory/        # Memory system
├── adk-runner/        # Execution runtime
├── adk-server/        # REST and A2A servers
├── adk-cli/           # CLI application
└── examples/          # Example applications
```

## 📋 Requirements Overview

### Functional Requirements (51 total)
- **FR-1**: Core Agent System (9 requirements)
  - Base agent trait, LLM agents, custom agents, agent composition, workflow agents
- **FR-2**: LLM Integration (6 requirements)
  - Model abstraction, Gemini integration, streaming, function calling
- **FR-3**: Tool System (7 requirements)
  - Tool trait, function tools, built-in tools, toolsets, MCP integration
- **FR-4**: Session Management (6 requirements)
  - Conversation state, events, state scoping, persistence
- **FR-5**: Artifact Management (5 requirements)
  - File storage, versioning, multiple backends
- **FR-6**: Memory System (4 requirements)
  - Long-term storage, semantic search
- **FR-7**: Runner/Execution (5 requirements)
  - Agent orchestration, context management, callbacks
- **FR-8**: Server Interfaces (5 requirements)
  - REST API, A2A protocol, endpoints
- **FR-9**: CLI/Launcher (4 requirements)
  - Interactive console, web UI, deployment modes

### Non-Functional Requirements (32 total)
- **NFR-1**: Performance - Zero-cost abstractions, efficient async
- **NFR-2**: Safety - Memory safety, type safety, explicit errors
- **NFR-3**: Concurrency - Tokio runtime, safe shared state
- **NFR-4**: API Design - Idiomatic Rust, builder patterns, traits
- **NFR-5**: Compatibility - Model providers, protocols
- **NFR-6**: Testability - Unit tests, integration tests, mocks
- **NFR-7**: Documentation - Rustdoc, examples, migration guide
- **NFR-8**: Deployment - Containerization, cloud-native

## 🗓️ Implementation Timeline

### Phase 1: Foundation (Weeks 1-2)
- Project setup, core traits, error types
- **Deliverable**: Core types and traits defined

### Phase 2: Session & Storage (Weeks 3-4)
- Session management, artifact, memory services
- **Deliverable**: Storage layer working

### Phase 3: Model Integration (Weeks 5-6)
- Gemini client, streaming, content generation
- **Deliverable**: LLM integration complete

### Phase 4: Tool System (Weeks 7-8)
- Function tools, built-in tools, toolsets
- **Deliverable**: Tool execution working

### Phase 5: Agent Implementation (Weeks 9-11)
- Custom, LLM, workflow agents
- **Deliverable**: All agent types implemented

### Phase 6: Runner & Execution (Weeks 12-13)
- Context management, callbacks, runner core
- **Deliverable**: End-to-end execution working

### Phase 7: Server & API (Weeks 14-16)
- REST API, A2A protocol, endpoints
- **Deliverable**: Servers operational

### Phase 8: CLI & Examples (Weeks 17-18)
- CLI, console mode, example applications
- **Deliverable**: CLI and examples working

### Phase 9: Advanced Features (Weeks 19-20)
- MCP integration, remote agent, advanced tools
- **Deliverable**: Advanced features complete

### Phase 10: Polish & Documentation (Weeks 21-22)
- Documentation, testing, optimization, deployment
- **Deliverable**: Production-ready release

**Total Duration**: 22 weeks (5.5 months) + 4 weeks buffer = **6.5 months**

## 🔑 Key Design Decisions

### 1. Trait-Based Architecture
```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    async fn run(&self, ctx: Arc<InvocationContext>) 
        -> Result<impl Stream<Item = Result<Event>>>;
}
```

### 2. Async-First with Tokio
- All I/O operations are async
- Use `futures::Stream` for event streaming
- Leverage Tokio's async runtime

### 3. Type-Safe Error Handling
```rust
#[derive(Debug, thiserror::Error)]
pub enum AdkError {
    #[error("Agent error: {0}")]
    Agent(String),
    #[error("Model error: {0}")]
    Model(String),
    // ... more variants
}
```

### 4. Builder Pattern for Configuration
```rust
let agent = LlmAgentBuilder::new("my_agent")
    .description("Agent description")
    .model(gemini_model)
    .tool(google_search)
    .build()?;
```

### 5. Streaming with Futures
```rust
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event>> + Send>>;
```

## 🚀 Getting Started (Future)

Once implemented, usage will look like:

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_tool::builtin::GoogleSearch;

#[tokio::main]
async fn main() -> Result<()> {
    let model = GeminiModel::new("gemini-2.5-flash").await?;
    
    let agent = LlmAgentBuilder::new("weather_agent")
        .description("Answers weather questions")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearch::new()))
        .build()?;
    
    let runner = Runner::new(RunnerConfig {
        agent: Arc::new(agent),
        session_service: Arc::new(InMemorySessionService::new()),
        ..Default::default()
    })?;
    
    let events = runner.run(ctx, user_id, session_id, user_message).await?;
    
    pin_mut!(events);
    while let Some(event) = events.next().await {
        println!("Event: {:?}", event?);
    }
    
    Ok(())
}
```

## 📊 Success Metrics

### Functional Completeness
- ✅ All 51 functional requirements implemented
- ✅ Feature parity with Go ADK
- ✅ All examples working

### Quality Metrics
- ✅ >80% code coverage
- ✅ Zero unsafe code (except where necessary)
- ✅ All clippy warnings resolved
- ✅ Documentation coverage >90%

### Performance Metrics
- ✅ Comparable or better latency than Go version
- ✅ Lower memory usage than Go version
- ✅ Efficient streaming (minimal buffering)

## 🛠️ Technology Stack

- **Language**: Rust 1.75+
- **Async Runtime**: Tokio
- **HTTP Server**: Axum or Actix-web
- **Serialization**: Serde
- **Database**: SQLite (sqlx or rusqlite)
- **Cloud Storage**: google-cloud-storage
- **CLI**: clap
- **Error Handling**: thiserror
- **Testing**: tokio::test, mockall

## 🤝 Contributing (Future)

Once the project is underway:
1. Review the requirements and design documents
2. Pick a task from the implementation plan
3. Follow Rust best practices and project conventions
4. Write tests for all new code
5. Update documentation

## 📄 License

This project will follow the same Apache 2.0 license as the original Go ADK.

## 🔗 Related Projects

- [ADK for Go](https://github.com/google/adk-go) - Original implementation
- [ADK for Python](https://github.com/google/adk-python) - Python version
- [ADK for Java](https://github.com/google/adk-java) - Java version
- [ADK Documentation](https://google.github.io/adk-docs/) - Official docs

## 📞 Contact

For questions about this conversion project, please open an issue in this repository.

---

**Note**: This is a planning repository. Implementation has not yet started. All code examples are illustrative of the intended API design.
