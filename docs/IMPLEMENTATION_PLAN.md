# Implementation Plan: Achieving ADK-Rust Feature Parity

## Goal Description
Transform `adk-rust` from a ~48% complete prototype into a production-ready implementation with full feature parity with `adk-go`. This requires fixing broken fundamentals, implementing missing callback systems, adding telemetry, and achieving 90%+ test coverage.

## User Review Required

> [!CAUTION]
> **Test Suite is Broken**: Tests don't compile. This must be fixed first before any new development.

> [!WARNING]
> **Fake Streaming**: Current implementation blocks on model calls, defeating the purpose of async/streaming architecture. High-priority fix.

> [!IMPORTANT]
> **Context Propagation**: Tools receive empty `user_id`/`session_id`. This breaks any tool that needs user context.

> [!WARNING]
> **Missing 52% of Features**: Callback system (6 types), instruction templating, structured I/O, telemetry all missing.

---

## Proposed Changes

### **Phase 1: Fix Broken Basics** (Priority: Critical, Est: 2 weeks)

#### [MODIFY] [session_api_tests.rs](file:///data/projects/adk-rust/adk-server/tests/session_api_tests.rs)
- Implement missing `list_agents()` and `root_agent()` for `AgentLoader` trait
- Fix compilation errors in test suite

#### [MODIFY] [llm_agent.rs](file:///data/projects/adk-rust/adk-agent/src/llm_agent.rs)
- **Critical**: Change `stream: false` to `stream: true` in line 252
- Handle partial chunks from `response_stream`
- Accumulate chunks for conversation history
- Yield partial events to user immediately
- Remove `SimpleToolContext` hack

#### [NEW] [agent_tool_context.rs](file:///data/projects/adk-rust/adk-agent/src/agent_tool_context.rs)
- Create proper `ToolContext` implementation that wraps parent `InvocationContext`
- Preserve `user_id`, `session_id`, `app_name`, etc.
- Provide access to `EventActions` for state mutations

---

### **Phase 2: Callbacks System** (Priority: High, Est: 3 weeks)

#### [MODIFY] [agent.rs](file:///data/projects/adk-rust/adk-core/src/agent.rs)
- Add `BeforeAgentCallback` and `AfterAgentCallback` types

#### [MODIFY] [lib.rs](file:///data/projects/adk-rust/adk-core/src/lib.rs)
- Add `BeforeModelCallback`, `AfterModelCallback` types
- Add `BeforeToolCallback`, `AfterToolCallback` types

#### [MODIFY] [llm_agent.rs](file:///data/projects/adk-rust/adk-agent/src/llm_agent.rs)
- Add fields for all 6 callback types to `LlmAgent` struct
- Add builder methods: `.before_model_callback()`, `.after_model_callback()`, etc.
- Integrate callbacks into agent run loop:
  - Before/after model calls
  - Before/after tool executions

#### [NEW] [tests/callback_tests.rs](file:///data/projects/adk-rust/adk-agent/tests/callback_tests.rs)
- Test each callback type independently
- Test callback short-circuiting (return early)
- Test error propagation

---

### **Phase 3: Instruction Templating** (Priority: High, Est: 2 weeks)

#### [NEW] [instruction_template.rs](file:///data/projects/adk-rust/adk-core/src/instruction_template.rs)
- Implement template parser for `{key}` placeholders
- Support `{artifact.name}` for artifact content injection
- Support `{key?}` for optional variables
- State resolution from `Session::State`

#### [MODIFY] [llm_agent.rs](file:///data/projects/adk-rust/adk-agent/src/llm_agent.rs)
- Add `instruction_provider: Option<InstructionProvider>` field
- Add `global_instruction: Option<String>` field
- Add `global_instruction_provider: Option<InstructionProvider>` field
- Process templates before sending to model

#### [NEW] [tests/instruction_template_tests.rs](file:///data/projects/adk-rust/adk-core/tests/instruction_template_tests.rs)
- Test placeholder substitution
- Test artifact injection
- Test optional placeholders
- Test missing variable errors

---

### **Phase 4: Structured I/O** (Priority: Medium, Est: 1 week)

#### [NEW] [schema.rs](file:///data/projects/adk-rust/adk-core/src/schema.rs)
- Define `Schema` struct matching Gemini's schema format
- Support object, array, string types
- Add schema validation helpers

#### [MODIFY] [llm_agent.rs](file:///data/projects/adk-rust/adk-agent/src/llm_agent.rs)
- Add `input_schema: Option<Schema>` field (for agent-as-tool)
- Add `output_schema: Option<Schema>` field
- Pass output schema to model request
- Validate responses against schema

---

### **Phase 5: Agent Control Features** (Priority: Medium, Est: 1 week)

#### [MODIFY] [llm_agent.rs](file:///data/projects/adk-rust/adk-agent/src/llm_agent.rs)
- Add `disallow_transfer_to_parent: bool` field
- Add `disallow_transfer_to_peers: bool` field
- Add `include_contents: IncludeContents` enum field
- Implement logic to filter conversation history based on `include_contents`

#### [NEW] [types.rs](file:///data/projects/adk-rust/adk-core/src/types.rs) (or extend existing)
- Add `IncludeContents` enum: `None`, `Default`

---

### **Phase 6: Telemetry** (Priority: High for Production, Est: 1 week)

#### [NEW] Crate: [adk-telemetry](file:///data/projects/adk-rust/adk-telemetry)
- Create new crate: `adk-telemetry/`
- Add `tracing` dependency
- Add `opentelemetry` integration
- Export macros: `trace!`, `debug!`, `info!`, `warn!`, `error!`

#### [MODIFY] [Cargo.toml](file:///data/projects/adk-rust/Cargo.toml)
- Add `adk-telemetry` to workspace members

#### [MODIFY] All core crates
- Add tracing to: agent execution, model calls, tool calls, session events
- Instrument async functions with `#[tracing::instrument]`

---

### **Phase 7: Test Coverage** (Priority: Critical, Est: 2 weeks)

#### Achieve 90%+ Coverage
- [NEW] `adk-agent/tests/streaming_test.rs` - Verify real streaming (partial events)
- [NEW] `adk-agent/tests/context_propagation_test.rs` - Verify tool receives correct user_id
- [NEW] `adk-agent/tests/callback_integration_test.rs` - All 6 callback types
- [NEW] `adk-core/tests/instruction_template_tests.rs` - Template system
- [NEW] `adk-agent/tests/schema_validation_test.rs` - Input/output schemas
- Expand existing tests in all crates

#### [NEW] [tests/integration_tests/](file:///data/projects/adk-rust/tests/integration_tests/)
- End-to-end tests with real Gemini API (optional, gated by env var)
- Multi-agent workflow tests
- MCP integration tests

---

### **Phase 8: Performance \u0026 Benchmarks** (Priority: Medium, Est: 1 week)

#### [NEW] [benches/](file:///data/projects/adk-rust/benches/)
- Benchmark agent execution latency
- Benchmark streaming throughput
- Compare with adk-go if possible

#### Optimization
- Profile with `cargo flamegraph`
- Reduce allocations in hot paths
- Optimize instruction template parsing

---

## Verification Plan

### **Automated Tests**
```bash
# Must pass:
cargo test --all
cargo clippy --all -- -D warnings
cargo fmt --all -- --check

# Coverage (requires tarpaulin):
cargo tarpaulin --all --out Html
# Target: 90%+ coverage
```

### **Manual Verification**
1. **Streaming**: `cargo run --example quickstart` - Verify incremental output
2. **Context**: Create a tool that prints `user_id`, verify it's not empty
3. **Callbacks**: Example with `BeforeModelCallback` that logs requests
4. **Templates**: Example with `{user_name}` in instruction
5. **Telemetry**: Enable tracing, verify spans in output

### **Performance**
```bash
cargo bench
# Compare latency vs adk-go (if possible)
```

---

## Timeline Summary

| Phase | Duration | Priority |
|-------|----------|----------|
| 1. Fix Broken Basics | 2 weeks | Critical |
| 2. Callbacks System | 3 weeks | High |
| 3. Instruction Templating | 2 weeks | High |
| 4. Structured I/O | 1 week | Medium |
| 5. Agent Control | 1 week | Medium |
| 6. Telemetry | 1 week | High |
| 7. Test Coverage | 2 weeks | Critical |
| 8. Performance | 1 week | Medium |
| **Total** | **~13 weeks** | - |
