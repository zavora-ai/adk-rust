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

## Phase 2: Function Calling & Multi-Turn ✅ COMPLETE

**Completed:** 2025-11-23

### Root Cause Analysis & Fixes:

**Problem 1: Tool Schema Missing**
- Tool trait had no methods to expose parameter/response schemas
- FunctionTool stored schemas but couldn't share them
- **Fix:** Added `parameters_schema()` and `response_schema()` to Tool trait

**Problem 2: Gemini Client Ignoring Function Parts**
- Client only processed `Part::Text`, ignored `FunctionCall` and `FunctionResponse`
- Model never saw tool results, causing infinite loops
- **Fix:** Proper content building using gemini-rust builder methods:
  - `with_user_message()` for user text
  - `with_message()` for model function calls  
  - `with_function_response()` for tool results

**Problem 3: Infinite Loop**
- Model kept calling tools without seeing responses
- No max iteration limit
- **Fix:** Added max 10 iterations, proper conversation history management

### Features Implemented:
- ✅ Multi-turn conversation loop
- ✅ Tool schema extraction from Tool trait
- ✅ Function call detection in model responses
- ✅ Tool execution with proper context
- ✅ Function response injection back to conversation
- ✅ Loop termination on text-only response
- ✅ Max iteration safety limit (10)

### Tests (5 passing with real Gemini):
- ✅ `test_llm_agent_builder` - validates builder
- ✅ `test_llm_agent_builder_missing_model` - error handling
- ✅ `test_llm_agent_basic_generation` - math question
- ✅ `test_llm_agent_with_instruction` - pirate speak
- ✅ `test_llm_agent_with_function_tool` - **get_current_time tool** ✨

### Example Flow (from test):
```
User: "What time is it right now?"
  ↓
Event 0: Model makes FunctionCall{get_current_time}
  ↓
Event 1: Tool executes, returns {"time": "2025-11-23T14:30:00Z"}
  ↓
Event 2: Model responds "It is 2025-11-23T14:30:00Z."
```

### Files Modified:
- `adk-core/src/tool.rs` - Added schema methods
- `adk-tool/src/function_tool.rs` - Implemented schema methods
- `adk-agent/src/llm_agent.rs` - Multi-turn loop (200+ lines)
- `adk-model/src/gemini/client.rs` - Proper function handling

### Workspace Tests: 59 (up from 58)

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
