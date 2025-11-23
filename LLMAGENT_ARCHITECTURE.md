# LlmAgent Architecture Analysis

## Overview
LlmAgent is the **core component** of ADK - it orchestrates LLM calls, tool execution, agent transfers, and conversation management. Understanding its architecture is critical for implementing the complete ADK system.

---

## Go Implementation Structure

### 1. **Two-Layer Architecture**

```
┌─────────────────────────────────────────┐
│         llmagent.go (Public API)        │
│  - Config struct (all options)          │
│  - New() constructor                    │
│  - Callbacks (before/after model/tool)  │
│  - run() method (delegates to Flow)     │
└─────────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────┐
│    base_flow.go (Execution Engine)      │
│  - Flow struct                          │
│  - Run() - outer loop                   │
│  - runOneStep() - single iteration      │
│  - Request/Response processors          │
│  - Tool execution                       │
│  - Agent transfer                       │
└─────────────────────────────────────────┘
```

### 2. **Key Components**

#### **llmagent.go** (Configuration Layer)
```go
type Config struct {
    // Identity
    Name, Description string
    SubAgents []agent.Agent
    
    // Model & Generation
    Model model.LLM
    GenerateContentConfig *genai.GenerateContentConfig
    
    // Instructions
    Instruction string                    // Template with {var} placeholders
    InstructionProvider InstructionProvider
    GlobalInstruction string              // For entire agent tree
    
    // Tools
    Tools []tool.Tool
    Toolsets []tool.Toolset
    
    // Callbacks
    BeforeModelCallbacks []BeforeModelCallback
    AfterModelCallbacks []AfterModelCallback
    BeforeToolCallbacks []BeforeToolCallback
    AfterToolCallbacks []AfterToolCallback
    BeforeAgentCallbacks []agent.BeforeAgentCallback
    AfterAgentCallbacks []agent.AfterAgentCallback
    
    // Schemas
    InputSchema *genai.Schema   // When agent used as tool
    OutputSchema *genai.Schema  // Structured output
    
    // Control
    DisallowTransferToParent bool
    DisallowTransferToPeers bool
    IncludeContents IncludeContents  // "none" or "default"
    OutputKey string  // Save output to session state
}
```

#### **base_flow.go** (Execution Engine)
```go
type Flow struct {
    Model model.LLM
    
    // Processor pipelines
    RequestProcessors []func(ctx, req) error
    ResponseProcessors []func(ctx, req, resp) error
    
    // Callbacks
    BeforeModelCallbacks []BeforeModelCallback
    AfterModelCallbacks []AfterModelCallback
    BeforeToolCallbacks []BeforeToolCallback
    AfterToolCallbacks []AfterToolCallback
}
```

---

## Execution Flow

### **Outer Loop: Run()**
```
┌─────────────────────────────────────────┐
│ Flow.Run() - Continues until done       │
└─────────────────────────────────────────┘
         │
         ▼
    ┌─────────────────┐
    │ runOneStep()    │ ◄──────┐
    └─────────────────┘        │
         │                     │
         ▼                     │
    Is final response?         │
         │                     │
    ┌────┴────┐               │
    │ Yes│ No │               │
    │    │    └───────────────┘
    ▼    │
  Done   │
         └─► Continue loop
```

### **Inner Loop: runOneStep()**
```
1. preprocess(req)
   ├─► basicRequestProcessor (config, output schema)
   ├─► instructionsRequestProcessor (inject instructions)
   ├─► ContentsRequestProcessor (conversation history)
   ├─► AgentTransferRequestProcessor (sub-agent tools)
   └─► toolPreprocess (tool-specific preprocessing)

2. callLLM(req)
   ├─► Run BeforeModelCallbacks
   ├─► Model.GenerateContent()
   └─► Run AfterModelCallbacks

3. postprocess(req, resp)
   └─► Run ResponseProcessors

4. Yield model response event

5. handleFunctionCalls(resp)
   ├─► For each function call:
   │   ├─► Run BeforeToolCallbacks
   │   ├─► tool.Run()
   │   └─► Run AfterToolCallbacks
   └─► Yield function response event

6. Handle agent transfer (if requested)
   └─► nextAgent.Run(ctx)
```

---

## Request Processors (Pipeline)

### **1. basicRequestProcessor**
- Copies `GenerateContentConfig` to request
- Sets `OutputSchema` if configured
- Sets `ResponseMIMEType = "application/json"` for structured output

### **2. instructionsRequestProcessor**
- Injects `Instruction` with template variable substitution
- Supports `{var_name}` and `{artifact.name}` placeholders
- Optional variables with `{var?}` syntax
- Handles `InstructionProvider` for dynamic instructions
- Injects `GlobalInstruction` from root agent

### **3. ContentsRequestProcessor**
- Builds conversation history from session events
- Filters based on `IncludeContents` setting
- Handles branch-specific history
- Converts events to genai.Content format

### **4. AgentTransferRequestProcessor**
- Creates tool declarations for sub-agents
- Enables agent-to-agent delegation
- Respects `DisallowTransferToParent` and `DisallowTransferToPeers`

### **5. toolPreprocess**
- Calls `ProcessRequest()` on each tool
- Allows tools to modify request (e.g., add grounding)

---

## Tool Execution

### **Function Call Handling**
```go
func handleFunctionCalls(tools, resp) (*Event, error) {
    for each FunctionCall in resp.Content {
        1. Find tool by name
        2. Create ToolContext with function call ID
        3. Run BeforeToolCallbacks
        4. Execute tool.Run(ctx, args)
        5. Run AfterToolCallbacks
        6. Build FunctionResponse event
    }
    
    // Merge parallel function responses
    return mergeParallelFunctionResponseEvents(events)
}
```

### **Tool Context**
- Contains `function_call_id` for matching responses
- Has `EventActions` for state delta
- Provides access to artifacts, memory, session

---

## Agent Transfer

### **How It Works**
1. Sub-agents exposed as tools via `AgentTransferRequestProcessor`
2. Model calls `transfer_to_agent` function
3. `handleFunctionCalls` detects transfer in `ev.Actions.TransferToAgent`
4. Flow finds target agent and calls `nextAgent.Run(ctx)`
5. Target agent's events are yielded through parent

### **Transfer Targets**
- Parent agent (unless `DisallowTransferToParent`)
- Sibling agents (unless `DisallowTransferToPeers`)
- Child agents (always allowed)

---

## Callbacks

### **Execution Order**
```
BeforeAgentCallbacks
    ↓
BeforeModelCallbacks
    ↓
Model.GenerateContent()
    ↓
AfterModelCallbacks
    ↓
BeforeToolCallbacks
    ↓
Tool.Run()
    ↓
AfterToolCallbacks
    ↓
AfterAgentCallbacks
```

### **Callback Semantics**
- **Before callbacks**: Can skip actual execution by returning non-nil result
- **After callbacks**: Can replace actual result
- **Chain stops** at first callback returning non-nil
- **Errors** propagate immediately

---

## State Management

### **OutputKey Feature**
```go
func maybeSaveOutputToState(event) {
    if OutputKey != "" && !event.Partial {
        // Extract text from event.Content.Parts
        text := extractTextParts(event.Content)
        
        // Save to state delta
        event.Actions.StateDelta[OutputKey] = text
    }
}
```

### **Use Cases**
- Extract agent output for later use
- Connect agents in workflows
- Pass data between agent invocations

---

## Streaming Support

### **Two Modes**
1. **SSE (Server-Sent Events)**: `StreamingMode = "sse"`
   - Model streams partial responses
   - Each chunk yields an event
   
2. **Non-streaming**: Default
   - Single complete response

### **Partial Events**
- `event.Partial = true` for intermediate chunks
- `event.Partial = false` for final response
- Only final response triggers state save

---

## Rust Implementation Comparison

### **Current Status (Phase 2 Complete)**

| Feature | Go | Rust | Status |
|---------|----|----- |--------|
| Basic generation | ✅ | ✅ | Complete |
| Instructions | ✅ | ✅ | Complete |
| Function calling | ✅ | ✅ | Complete |
| Multi-turn loop | ✅ | ✅ | Complete |
| Tool execution | ✅ | ✅ | Complete |
| Conversation history | ✅ | ✅ | Complete |
| Max iterations | ✅ | ✅ | Complete |
| Request processors | ✅ | ❌ | Missing |
| Response processors | ✅ | ❌ | Missing |
| Callbacks | ✅ | ❌ | Missing |
| Agent transfer | ✅ | ❌ | Missing |
| Template variables | ✅ | ❌ | Missing |
| OutputKey | ✅ | ❌ | Missing |
| Streaming | ✅ | ❌ | Missing |
| OutputSchema | ✅ | ❌ | Missing |

### **Architecture Differences**

#### **Go: Two-Layer Design**
```
llmagent.go (config) → base_flow.go (execution)
```
- Clean separation of concerns
- Flow is reusable for different agent types
- Processors are pluggable

#### **Rust: Single-Layer Design**
```
llm_agent.rs (config + execution combined)
```
- Simpler for basic use cases
- Harder to extend with processors
- Less modular

---

## Key Insights

### **1. Processor Pipeline is Critical**
The request/response processor architecture enables:
- **Modularity**: Each feature is a processor
- **Extensibility**: Add new processors without changing core
- **Testability**: Test processors independently
- **Order control**: Processors run in specific sequence

### **2. Callbacks Enable Customization**
Four callback points provide:
- **Caching**: BeforeModel can return cached response
- **Logging**: AfterModel logs all responses
- **Validation**: BeforeTool validates arguments
- **Transformation**: AfterTool transforms results

### **3. Agent Transfer is Tool-Based**
- Sub-agents are exposed as tools
- Model decides when to transfer
- Enables hierarchical agent trees
- Supports delegation patterns

### **4. State Management is Event-Driven**
- All state changes via `EventActions.StateDelta`
- Events carry state mutations
- Session service applies deltas
- Enables time-travel debugging

### **5. Streaming Requires Special Handling**
- Partial events during streaming
- Final event triggers state save
- Need to aggregate chunks
- Handle max token limits

---

## Rust Implementation Roadmap

### **Phase 3: Advanced Features** (Next)

#### **3.1 Processor Architecture**
```rust
pub trait RequestProcessor {
    async fn process(&self, ctx: &InvocationContext, req: &mut LlmRequest) -> Result<()>;
}

pub trait ResponseProcessor {
    async fn process(&self, ctx: &InvocationContext, req: &LlmRequest, resp: &mut LlmResponse) -> Result<()>;
}

pub struct Flow {
    model: Arc<dyn Llm>,
    request_processors: Vec<Box<dyn RequestProcessor>>,
    response_processors: Vec<Box<dyn ResponseProcessor>>,
    // ... callbacks
}
```

#### **3.2 Template Variables**
```rust
fn inject_template_variables(instruction: &str, ctx: &InvocationContext) -> Result<String> {
    // Replace {var_name} with session state
    // Replace {artifact.name} with artifact content
    // Handle {var?} optional syntax
}
```

#### **3.3 Callbacks**
```rust
pub type BeforeModelCallback = Box<dyn Fn(&CallbackContext, &LlmRequest) -> Result<Option<LlmResponse>>>;
pub type AfterModelCallback = Box<dyn Fn(&CallbackContext, &LlmResponse) -> Result<Option<LlmResponse>>>;
// ... similar for tools
```

#### **3.4 Agent Transfer**
```rust
// AgentTool wraps sub-agents as tools
pub struct AgentTool {
    agent: Arc<dyn Agent>,
}

impl Tool for AgentTool {
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        // Run sub-agent and return its output
    }
}
```

#### **3.5 OutputKey & State Management**
```rust
fn maybe_save_output_to_state(&self, event: &mut Event) {
    if let Some(key) = &self.output_key {
        if !event.partial {
            let text = extract_text_parts(&event.content);
            event.actions.state_delta.insert(key.clone(), text);
        }
    }
}
```

---

## Testing Strategy

### **Unit Tests**
- Each processor independently
- Callback execution order
- Template variable substitution
- State delta application

### **Integration Tests**
- Full multi-turn conversations
- Tool execution with callbacks
- Agent transfer scenarios
- Streaming responses

### **Real API Tests**
- Continue using actual Gemini API
- Validates end-to-end behavior
- Catches API changes early

---

## Performance Considerations

### **Parallel Tool Execution**
Go implementation executes tools sequentially. Could parallelize:
```rust
let futures: Vec<_> = function_calls.iter()
    .map(|fc| execute_tool(fc))
    .collect();
let results = futures::future::join_all(futures).await;
```

### **Streaming Optimization**
- Minimize buffering
- Yield events immediately
- Use `async_stream` for clean syntax

### **Memory Management**
- Use `Arc` for shared data
- Clone only when necessary
- Leverage Rust's ownership for safety

---

## Conclusion

LlmAgent is the **heart of ADK**. Its architecture demonstrates:

1. **Modularity**: Processor pipeline enables clean feature separation
2. **Flexibility**: Callbacks allow customization at every step
3. **Composability**: Agent-as-tool pattern enables hierarchies
4. **Observability**: Event-driven design enables debugging
5. **Extensibility**: New features add processors, not core changes

The Rust implementation has a solid foundation (Phase 2 complete) but needs:
- Processor architecture (Phase 3)
- Callback system (Phase 3)
- Agent transfer (Phase 3)
- Template variables (Phase 3)
- Streaming support (Phase 3)

Once these are implemented, the Rust ADK will have feature parity with Go.
