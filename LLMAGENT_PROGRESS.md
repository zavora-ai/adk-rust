# LlmAgent Implementation Progress

## Overview
LlmAgent is the most critical agent type - it wraps an LLM model and orchestrates tool execution, multi-turn conversations, and agent transfers.

## ⚠️ CRITICAL LIMITATION: Tool Type Mixing

**GenAI API Limitation:** Cannot mix different tool types in a single agent!

### The Problem:
- ❌ **Cannot** have both `GoogleSearch` (Gemini built-in) AND `FunctionTool` (custom) in same agent
- ❌ **Cannot** mix different Gemini tool types (search, code execution, etc.)

### The Solution: Agent-as-Tool Pattern
- ✅ Create **separate sub-agents** for each tool type
- ✅ Use **AgentTool** to wrap sub-agents as tools
- ✅ Root agent delegates to sub-agents via tool calls

### Example Architecture:
```
Root Agent (no direct tools)
├── SearchAgent (GoogleSearch only)
├── PoemAgent (FunctionTool only)
└── CodeAgent (CodeExecution only)
```

### Implementation Status:
- ✅ GoogleSearchTool marked as Gemini-internal (cannot execute locally)
- ✅ FunctionTool supports custom functions
- ❌ **AgentTool not yet implemented** (Phase 3)
- ❌ Agent transfer not yet implemented (Phase 3)

### Current Workaround:
For Phase 2, we'll test with **single tool type per agent**:
- Test 1: Agent with FunctionTool only
- Test 2: Agent with GoogleSearch only (when Gemini supports it)
- Phase 3: Implement AgentTool for proper composition

---

## Phase 1: Core LlmAgent ✅ COMPLETE

**Completed:** 2025-11-23

### Features Implemented:
- ✅ LlmAgent struct with model, instruction, tools, sub-agents
- ✅ LlmAgentBuilder with builder pattern
- ✅ Basic run loop (single-turn, text-only)
- ✅ Instruction support (system message injection)
- ✅ Real Gemini integration (all tests use actual API)
- ✅ Event generation with author and content
- ✅ Manual Debug implementation for trait objects

### Tests (4 passing):
- ✅ `test_llm_agent_builder` - validates builder pattern
- ✅ `test_llm_agent_builder_missing_model` - error handling
- ✅ `test_llm_agent_basic_generation` - math question (2+2=4)
- ✅ `test_llm_agent_with_instruction` - pirate speak instruction

### Files Created:
- `adk-agent/src/llm_agent.rs` (155 lines)
- `adk-agent/tests/llm_agent_tests.rs` (165 lines)

### Workspace Tests: 58 (up from 54)

---

## Phase 2: Function Calling & Multi-Turn 🔄 IN PROGRESS

**Started:** 2025-11-23

### Goals:
1. Multi-turn conversation loop
2. Function calling support
3. Tool execution (parallel)
4. Tool response handling
5. Loop termination detection

### Implementation Plan:

#### 2.1 Multi-Turn Loop
```rust
async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
    loop {
        // Build request with tools
        // Call model
        // If text -> yield and break
        // If function calls -> execute and continue
    }
}
```

#### 2.2 Tool Execution
- Extract function calls from LlmResponse
- Execute tools in parallel (tokio::spawn)
- Collect results
- Add to conversation history

#### 2.3 Events
- Model response event (with function calls)
- Tool execution events (one per tool)
- Final text response event

### Tests Needed:
- [ ] Function calling with GoogleSearch
- [ ] Multi-turn conversation (tool -> response)
- [ ] Parallel tool execution
- [ ] Loop termination

### Current Limitations:
- ❌ No function calling
- ❌ No multi-turn loop
- ❌ No tool execution
- ❌ No agent transfer
- ❌ No streaming
- ❌ No template variables
- ❌ No callbacks

---

## Phase 3: Advanced Features ⏳ PLANNED

### Goals:
1. Template variable injection (`{var_name}`, `{artifact.name}`)
2. Callbacks (before/after model, before/after tool)
3. Agent transfer (delegate to sub-agents)
4. Output schema validation
5. State management (output_key)
6. Streaming support
7. Session history integration

### Tests Needed:
- [ ] Template variables in instructions
- [ ] Callbacks execution
- [ ] Agent transfer
- [ ] Output schema
- [ ] State management
- [ ] Streaming responses

---

## Architecture Notes

### From Go Implementation:
- `llmagent.go` - Config, builder, agent wrapper
- `base_flow.go` - Flow struct with Run() loop
- Request processors: basic, contents, instructions, agent transfer
- Response processors: function calls, agent transfer, output saving

### Rust Design:
- LlmAgent implements Agent trait
- Uses Arc<dyn Llm> for model abstraction
- Tools stored as Vec<Arc<dyn Tool>>
- Sub-agents as Vec<Arc<dyn Agent>>
- Event streaming via async_stream

### Key Differences:
- Go uses `iter.Seq2` for events, Rust uses `Stream`
- Go has separate Flow struct, Rust integrates into run()
- Rust uses Arc for shared ownership vs Go's pointers

---

## Next Steps

1. **Implement multi-turn loop** in run()
2. **Add tool execution** with parallel spawning
3. **Handle function calls** from LlmResponse
4. **Test with GoogleSearch** tool
5. **Verify loop termination** logic

---

## References

- Requirements: FR-1.2, FR-2.5
- Design: D-1, D-7 (Agent Layer, Builder Pattern)
- Go: `adk-go/agent/llmagent/llmagent.go`
- Go: `adk-go/internal/llminternal/base_flow.go`
- Implementation Plan: Task 5.2
