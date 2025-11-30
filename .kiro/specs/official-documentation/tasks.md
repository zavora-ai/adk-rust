# Implementation Plan

## Coding Conventions (IMPORTANT)

All examples MUST follow these patterns established in Phase 2:

### Return Type
Use `std::result::Result<(), Box<dyn std::error::Error>>` instead of `Result<()>` to avoid conflicts with ADK's `Result<T>` type alias:
```rust
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // ...
}
```

### Example Pattern
All examples should support both validation and interactive modes:
```rust
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};

if is_interactive_mode() {
    Launcher::new(Arc::new(agent)).run().await?;
} else {
    print_validating("page.md");
    // Validation logic...
    print_success("example_name");
    println!("\nTip: Run with 'chat' for interactive mode:");
    println!("  cargo run --example NAME -p adk-rust-guide -- chat");
}
```

### Run Modes
- `cargo run --example NAME -p adk-rust-guide` - Validation mode (default)
- `cargo run --example NAME -p adk-rust-guide -- chat` - Interactive console
- `cargo run --example NAME -p adk-rust-guide -- serve` - Web server mode

---

## Phase 1: Foundation and Structure

- [x] 1. Set up documentation structure and validation framework
  - [x] 1.1 Create documentation folder structure in `docs/official_docs/`
    - Create folders: agents/, tools/, sessions/, callbacks/, artifacts/, events/, observability/, deployment/
    - Create `docs/roadmap/` folder for unimplemented features
    - _Requirements: 1.1, 1.2, 1.3_
  - [x] 1.2 Create index.md table of contents page
    - List all documentation sections with links
    - Include brief descriptions for each section
    - _Requirements: 1.4_
  - [x] 1.3 Restructure adk-rust-guide for validation examples
    - Update Cargo.toml with example configurations
    - Create examples/ folder structure matching docs
    - Create shared utilities in src/lib.rs
    - _Requirements: 16.1_
  - [x] 1.4 Write property test for documentation-example correspondence
    - **Property 2: Documentation-Example Correspondence**
    - **Validates: Requirements 16.1, 16.2**

## Phase 2: Introduction and Quickstart

- [x] 2. Create introduction and quickstart documentation
  - [x] 2.1 Write introduction.md
    - Explain ADK-Rust purpose and architecture
    - Document key concepts (agents, tools, sessions, etc.)
    - _Requirements: 2.1_
  - [x] 2.2 Write quickstart.md with complete working example
    - Step-by-step instructions for first agent
    - Include GOOGLE_API_KEY setup instructions
    - Complete runnable code sample
    - _Requirements: 2.2, 2.3, 2.4_
  - [x] 2.3 Create quickstart validation example
    - Create examples/quickstart.rs in adk-rust-guide
    - Verify example compiles and runs
    - _Requirements: 2.5_
  - [x] 2.4 Write property test for quickstart example compilation
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 2.3, 2.5**

- [ ] 3. Checkpoint - Ensure all tests pass
  - Ensure all tests pass, ask the user if questions arise.

## Phase 3: LlmAgent Documentation

- [x] 4. Create LlmAgent documentation
  - [x] 4.1 Write agents/llm-agent.md
    - Document all builder methods (name, description, model, instruction, tools, etc.)
    - Explain instruction templating with {var} syntax
    - Document IncludeContents enum options
    - _Requirements: 3.1, 3.2, 3.3, 3.4_
  - [x] 4.2 Create LlmAgent basic example
    - Create examples/agents/llm_agent_basic.rs
    - Demonstrate minimal agent creation
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 3.5_
  - [x] 4.3 Create LlmAgent configuration example
    - Create examples/agents/llm_agent_config.rs
    - Demonstrate all configuration options
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 3.2, 3.5_
  - [x] 4.4 Write property test for LlmAgent examples
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 3.2, 3.5**

## Phase 4: Workflow Agents Documentation

- [x] 5. Create workflow agents documentation
  - [x] 5.1 Write agents/workflow-agents.md
    - Document SequentialAgent with examples
    - Document ParallelAgent with examples
    - Document LoopAgent with examples
    - Explain ExitLoopTool for loop control
    - _Requirements: 4.1, 4.2, 4.3, 4.4_
  - [x] 5.2 Create SequentialAgent validation example
    - Create examples/agents/sequential_agent.rs
    - Demonstrate multi-step pipeline
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 4.5_
  - [x] 5.3 Create ParallelAgent validation example
    - Create examples/agents/parallel_agent.rs
    - Demonstrate concurrent execution
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 4.5_
  - [x] 5.4 Create LoopAgent validation example
    - Create examples/agents/loop_agent.rs
    - Demonstrate iterative refinement with ExitLoopTool
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 4.5_
  - [x] 5.5 Write property test for workflow agent examples
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 4.1, 4.2, 4.3, 4.5**

- [ ] 6. Checkpoint - Ensure all tests pass
  - Ensure all tests pass, ask the user if questions arise.

## Phase 5: Tools Documentation

- [x] 7. Create function tools documentation
  - [x] 7.1 Write tools/function-tools.md
    - Explain FunctionTool creation with async functions
    - Document parameter handling via serde_json::Value
    - Explain return value conventions (Result<Value, AdkError>)
    - Document ToolContext interface
    - _Requirements: 5.1, 5.2, 5.3, 5.5_
  - [x] 7.2 Create function tool validation example
    - Create examples/tools/function_tool.rs
    - Demonstrate custom tool with parameters
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 5.4, 5.6_
  - [x] 7.3 Write property test for function tool example
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 5.4, 5.6**

- [x] 8. Create built-in tools documentation
  - [x] 8.1 Write tools/built-in-tools.md
    - Document GoogleSearchTool with usage examples
    - Document ExitLoopTool for loop control
    - _Requirements: 6.1, 6.2_
  - [x] 8.2 Create built-in tools validation example
    - Create examples/tools/google_search.rs
    - Demonstrate GoogleSearchTool usage
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 6.4_
  - [x] 8.3 Write property test for built-in tools example
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 6.4**

## Phase 6: Sessions Documentation

- [x] 9. Create sessions documentation
  - [x] 9.1 Write sessions/sessions.md
    - Explain Session trait and properties
    - Document SessionService implementations (InMemory, Database)
    - Explain session lifecycle
    - _Requirements: 7.1, 7.2, 7.3_
  - [x] 9.2 Write sessions/state.md
    - Document session state management
    - Explain state key prefixes (app:, user:, temp:)
    - _Requirements: 7.4, 7.5_
  - [x] 9.3 Create session basic validation example
    - Create examples/sessions/session_basic.rs
    - Demonstrate session creation and retrieval
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 7.6_
  - [x] 9.4 Create state management validation example
    - Create examples/sessions/state_management.rs
    - Demonstrate state operations with prefixes
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 7.6_
  - [x] 9.5 Write property test for session examples
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 7.6**

- [ ] 10. Checkpoint - Ensure all tests pass
  - Ensure all tests pass, ask the user if questions arise.

## Phase 7: Callbacks Documentation

- [x] 11. Create callbacks documentation
  - [x] 11.1 Write callbacks/callbacks.md
    - Document all callback types (before/after agent, model, tool)
    - Explain callback return value semantics
    - Document CallbackContext interface
    - Provide examples for logging, guardrails, caching
    - _Requirements: 8.1, 8.2, 8.3, 8.4_
  - [x] 11.2 Create before_agent callback example
    - Create examples/callbacks/before_agent.rs
    - Demonstrate agent interception
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 8.5_
  - [x] 11.3 Create after_agent callback example
    - Create examples/callbacks/after_agent.rs
    - Demonstrate response modification
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 8.5_
  - [x] 11.4 Create model callbacks example
    - Create examples/callbacks/model_callbacks.rs
    - Demonstrate before/after model callbacks
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 8.5_
  - [x] 11.5 Create tool callbacks example
    - Create examples/callbacks/tool_callbacks.rs
    - Demonstrate before/after tool callbacks
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 8.5_
  - [x] 11.6 Write property test for callback examples
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 8.3, 8.5**

## Phase 8: Artifacts Documentation

- [x] 12. Create artifacts documentation
  - [x] 12.1 Write artifacts/artifacts.md
    - Explain Artifacts trait and Part representation
    - Document ArtifactService implementations
    - Explain artifact operations (save, load, list, delete)
    - Document versioning and namespace scoping
    - _Requirements: 9.1, 9.2, 9.3, 9.4, 9.5_
  - [x] 12.2 Create artifact operations validation example
    - Create examples/artifacts/artifact_ops.rs
    - Demonstrate save and load operations
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 9.6_
  - [x] 12.3 Write property test for artifact example
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 9.6**

## Phase 9: Events Documentation

- [x] 13. Create events documentation
  - [x] 13.1 Write events/events.md
    - Explain Event struct and fields
    - Document EventActions and state_delta
    - Explain conversation history formation
    - _Requirements: 10.1, 10.2, 10.3_
  - [x] 13.2 Create event inspection validation example
    - Create examples/events/event_inspection.rs
    - Demonstrate event handling and inspection
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 10.4_
  - [x] 13.3 Write property test for event example
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 10.4**

- [x] 14. Checkpoint - Ensure all tests pass
  - Ensure all tests pass, ask the user if questions arise.

## Phase 10: Multi-Agent Documentation

- [x] 15. Create multi-agent documentation
  - [x] 15.1 Write agents/multi-agent.md
    - Explain sub_agents configuration
    - Document agent transfer behavior
    - Explain global_instruction for tree-wide config
    - _Requirements: 11.1, 11.2, 11.3_
  - [x] 15.2 Create multi-agent validation example
    - Create examples/agents/multi_agent.rs
    - Demonstrate sub-agent hierarchy
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 11.6_
  - [x] 15.3 Write property test for multi-agent example
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 11.6**

## Phase 11: Observability Documentation

- [x] 16. Create observability documentation
  - [x] 16.1 Write observability/telemetry.md
    - Document adk_telemetry crate
    - Explain log levels and structured logging
    - Provide telemetry configuration examples
    - _Requirements: 15.1, 15.2_
  - [x] 16.2 Create telemetry validation example
    - Create examples/observability/telemetry.rs
    - Demonstrate telemetry setup
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 15.3, 15.4_
  - [x] 16.3 Write property test for telemetry example
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 15.3, 15.4**

## Phase 12: Deployment Documentation

- [x] 17. Create deployment documentation
  - [x] 17.1 Write deployment/launcher.md
    - Document Launcher API
    - Explain console and server modes
    - _Requirements: 13.1_
  - [x] 17.2 Write deployment/server.md
    - Document REST API endpoints
    - Explain web UI integration
    - _Requirements: 13.2, 13.3_
  - [x] 17.3 Create console mode validation example
    - Create examples/deployment/console_mode.rs
    - Demonstrate console runner
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 13.4, 13.5_
  - [x] 17.4 Create server mode validation example
    - Create examples/deployment/server_mode.rs
    - Demonstrate HTTP server
    - Follow coding conventions: use `std::result::Result`, support validation/interactive modes
    - _Requirements: 13.4, 13.5_
  - [x] 17.5 Write property test for deployment examples
    - **Property 1: Documentation Example Compilation**
    - **Validates: Requirements 13.4, 13.5**

- [ ] 18. Checkpoint - Ensure all tests pass
  - Ensure all tests pass, ask the user if questions arise.

## Phase 13: MCP and A2A Documentation

- [ ] 19. Create MCP documentation
  - [ ] 19.1 Assess MCP implementation status
    - Review current McpToolset implementation
    - Determine what features are functional vs roadmap
    - _Requirements: 12.1, 12.2_
  - [ ] 19.2 Write tools/mcp-tools.md (or roadmap/mcp-tools.md)
    - Document implemented MCP features
    - Place unimplemented features in roadmap
    - _Requirements: 12.3_
  - [ ] 19.3 Create MCP validation example (if applicable)
    - Create examples/tools/mcp_tool.rs if MCP is functional
    - _Requirements: 12.4_

- [ ] 20. Create A2A documentation
  - [ ] 20.1 Assess A2A implementation status
    - Review current A2A protocol implementation
    - Determine what features are functional vs roadmap
    - _Requirements: 14.1, 14.2_
  - [ ] 20.2 Write deployment/a2a.md (or roadmap/a2a.md)
    - Document implemented A2A features
    - Place unimplemented features in roadmap
    - _Requirements: 14.3_
  - [ ] 20.3 Create A2A validation example (if applicable)
    - Create examples/deployment/a2a.rs if A2A is functional
    - _Requirements: 14.4_

## Phase 14: Roadmap Documentation

- [ ] 21. Create roadmap documentation for unimplemented features
  - [ ] 21.1 Write roadmap/long-running-tools.md
    - Describe planned Long Running Function Tools API
    - No runnable code samples
    - _Requirements: 17.1, 17.2, 17.3, 17.4, 17.5_
  - [ ] 21.2 Write roadmap/vertex-ai-session.md
    - Describe planned VertexAI Session Service
    - No runnable code samples
    - _Requirements: 17.1, 17.2, 17.3, 17.4, 17.5_
  - [ ] 21.3 Write roadmap/gcs-artifacts.md
    - Describe planned GCS Artifact Service
    - No runnable code samples
    - _Requirements: 17.1, 17.2, 17.3, 17.4, 17.5_
  - [ ] 21.4 Write roadmap/agent-tool.md
    - Describe planned Agent-as-a-Tool pattern
    - No runnable code samples
    - _Requirements: 17.1, 17.2, 17.3, 17.4, 17.5_
  - [ ] 21.5 Write roadmap/evaluation.md
    - Describe planned Evaluation Framework
    - No runnable code samples
    - _Requirements: 17.1, 17.2, 17.3, 17.4, 17.5_
  - [ ] 21.6 Write property test for roadmap no runnable code
    - **Property 4: Roadmap Files No Runnable Code**
    - **Validates: Requirements 17.4**

## Phase 15: Validation and Quality Assurance

- [ ] 22. Create comprehensive validation tests
  - [ ] 22.1 Write property test for no mocks in examples
    - **Property 3: No Mocks in Validated Examples**
    - **Validates: Requirements 16.5**
  - [ ] 22.2 Write property test for required imports
    - **Property 5: Required Imports Present**
    - **Validates: Requirements 18.2**
  - [ ] 22.3 Create validation test runner script
    - Script to compile all examples
    - Report validation status for each doc page
    - _Requirements: 16.3_

- [ ] 23. Final review and documentation polish
  - [ ] 23.1 Review all documentation for consistency
    - Ensure consistent formatting and style
    - Verify all cross-references work
    - _Requirements: 18.1, 18.4, 18.5_
  - [ ] 23.2 Update index.md with final structure
    - Ensure all pages are linked
    - Add validation status indicators
    - _Requirements: 1.4_
  - [ ] 23.3 Create README for adk-rust-guide
    - Document how to run validation examples
    - Explain validation process
    - _Requirements: 16.3_

- [ ] 24. Final Checkpoint - Ensure all tests pass
  - Ensure all tests pass, ask the user if questions arise.
