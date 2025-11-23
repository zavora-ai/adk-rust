# Task 7.5: A2A Protocol - Full Implementation Plan

## Goal
Achieve full feature parity with Go's `adk-go/server/adka2a` implementation.

## Scope: ~2000 lines across 6 modules

### Module 1: Agent Card Generation
**File**: `adk-server/src/a2a/agent_card.rs` (~400 lines)

**Functions**:
```rust
pub fn build_agent_skills(agent: &dyn Agent) -> Vec<AgentSkill>
fn build_primary_skills(agent: &dyn Agent) -> Vec<AgentSkill>
fn build_sub_agent_skills(agent: &dyn Agent) -> Vec<AgentSkill>
fn build_llm_agent_skills(agent: &dyn Agent) -> Vec<AgentSkill>
fn build_workflow_agent_skills(agent: &dyn Agent) -> Vec<AgentSkill>
fn build_agent_description(agent: &dyn Agent) -> String
```

**Dependencies**: `a2a-rs` for AgentSkill type

### Module 2: Part Conversion
**File**: `adk-server/src/a2a/parts.rs` (~300 lines)

**Functions**:
```rust
pub fn adk_parts_to_a2a(parts: &[Part], long_running_ids: &[String]) -> Result<Vec<a2a_rs::Part>>
pub fn a2a_parts_to_adk(parts: &[a2a_rs::Part]) -> Result<Vec<Part>>
fn convert_function_call(call: &FunctionCall, is_long_running: bool) -> a2a_rs::Part
fn convert_function_response(response: &FunctionResponse) -> a2a_rs::Part
```

**Conversions**:
- Text → TextPart
- InlineData → FilePart (base64)
- FunctionCall → DataPart (JSON)
- FunctionResponse → DataPart (JSON)

### Module 3: Event Conversion
**File**: `adk-server/src/a2a/events.rs` (~350 lines)

**Functions**:
```rust
pub fn event_to_message(event: &Event) -> Result<a2a_rs::Message>
pub fn message_to_event(ctx: &InvocationContext, msg: &a2a_rs::Message) -> Result<Event>
pub fn task_to_event(ctx: &InvocationContext, task: &a2a_rs::Task) -> Result<Event>
pub fn artifact_to_event(ctx: &InvocationContext, artifact: &a2a_rs::Artifact) -> Result<Event>
fn extract_metadata(event: &Event) -> HashMap<String, Value>
fn set_actions_metadata(meta: &mut HashMap<String, Value>, actions: &EventActions)
```

**Metadata Keys**:
- `adk_task_id`
- `adk_context_id`
- `adk_escalate`
- `adk_transfer_to_agent`
- `adk_invocation_id`
- `adk_author`
- `adk_branch`

### Module 4: Event Processor
**File**: `adk-server/src/a2a/processor.rs` (~400 lines)

**Main Type**:
```rust
pub struct EventProcessor {
    req_ctx: RequestContext,
    meta: InvocationMeta,
    terminal_actions: EventActions,
    response_id: Option<String>,
    terminal_events: HashMap<TaskState, TaskStatusUpdateEvent>,
}

impl EventProcessor {
    pub fn new(req_ctx: RequestContext, meta: InvocationMeta) -> Self
    pub async fn process(&mut self, event: &Event) -> Result<Option<TaskArtifactUpdateEvent>>
    pub fn make_terminal_events(&self) -> Vec<Event>
    fn update_terminal_actions(&mut self, event: &Event)
    fn is_input_required(event: &Event) -> bool
}
```

**Logic**:
1. Process each ADK event
2. Check for errors → queue TaskStateFailed
3. Check for long-running tools → queue TaskStateInputRequired
4. Convert parts to A2A
5. Create/update artifact
6. Generate terminal events

### Module 5: Executor
**File**: `adk-server/src/a2a/executor.rs` (~400 lines)

**Main Type**:
```rust
pub struct Executor {
    config: ExecutorConfig,
}

pub struct ExecutorConfig {
    pub runner_config: RunnerConfig,
    pub run_config: RunConfig,
}

impl Executor {
    pub fn new(config: ExecutorConfig) -> Self
    pub async fn execute(&self, ctx: RequestContext, queue: EventQueue) -> Result<()>
    pub async fn cancel(&self, ctx: RequestContext, queue: EventQueue) -> Result<()>
    async fn process(&self, runner: &Runner, processor: &mut EventProcessor, content: Content, queue: &EventQueue) -> Result<()>
    async fn prepare_session(&self, meta: &InvocationMeta) -> Result<()>
}
```

**Execution Flow**:
1. Convert A2A message to ADK content
2. Create runner
3. Check/create session
4. Send TaskStateSubmitted
5. Send TaskStateWorking
6. Stream ADK events → A2A artifacts
7. Send terminal event

### Module 6: Metadata Management
**File**: `adk-server/src/a2a/metadata.rs` (~150 lines)

**Types**:
```rust
pub struct InvocationMeta {
    pub user_id: String,
    pub session_id: String,
    pub event_meta: HashMap<String, Value>,
}

pub fn to_invocation_meta(config: &ExecutorConfig, req_ctx: &RequestContext) -> InvocationMeta
pub fn to_event_meta(meta: &InvocationMeta, event: &Event) -> Result<HashMap<String, Value>>
pub fn set_actions_meta(meta: HashMap<String, Value>, actions: &EventActions) -> HashMap<String, Value>
pub fn to_a2a_meta_key(key: &str) -> String  // Adds "adk_" prefix
```

## Dependencies

### Add to `adk-server/Cargo.toml`:
```toml
[dependencies]
a2a-rs = { version = "0.3", default-features = false, features = ["domain"] }
base64 = "0.21"
```

## Implementation Phases

### Phase 1: Setup & Types (1 hour)
- [ ] Add `a2a-rs` dependency
- [ ] Create module structure
- [ ] Define core types (InvocationMeta, ExecutorConfig, etc.)

### Phase 2: Part Conversion (2 hours)
- [ ] Implement `adk_parts_to_a2a()`
- [ ] Implement `a2a_parts_to_adk()`
- [ ] Handle all part types
- [ ] Write conversion tests

### Phase 3: Event Conversion (2 hours)
- [ ] Implement `event_to_message()`
- [ ] Implement `message_to_event()`
- [ ] Implement `task_to_event()`
- [ ] Implement `artifact_to_event()`
- [ ] Write conversion tests

### Phase 4: Metadata Management (1 hour)
- [ ] Implement `InvocationMeta`
- [ ] Implement metadata conversion functions
- [ ] Write metadata tests

### Phase 5: Event Processor (2 hours)
- [ ] Implement `EventProcessor` struct
- [ ] Implement `process()` method
- [ ] Implement `make_terminal_events()`
- [ ] Handle error states
- [ ] Write processor tests

### Phase 6: Executor (2 hours)
- [ ] Implement `Executor` struct
- [ ] Implement `execute()` method
- [ ] Implement `cancel()` method
- [ ] Session management
- [ ] Write executor tests

### Phase 7: Agent Card Generation (2 hours)
- [ ] Implement `build_agent_skills()`
- [ ] Implement LLM agent skill extraction
- [ ] Implement workflow agent skill extraction
- [ ] Recursive sub-agent processing
- [ ] Write skill extraction tests

### Phase 8: Integration & Testing (2 hours)
- [ ] Integration tests
- [ ] End-to-end tests
- [ ] Documentation
- [ ] Examples

**Total Estimated Time**: 14 hours

## Testing Strategy

### Unit Tests (per module):
1. **parts.rs**: Test each conversion type
2. **events.rs**: Test event/message conversions
3. **metadata.rs**: Test metadata extraction
4. **processor.rs**: Test event processing logic
5. **executor.rs**: Test execution flow
6. **agent_card.rs**: Test skill extraction

### Integration Tests:
1. Full A2A request → ADK execution → A2A response
2. Error handling
3. Long-running tools
4. Sub-agent orchestration

## File Structure

```
adk-server/src/a2a/
├── mod.rs              # Module exports
├── agent_card.rs       # AgentCard generation
├── parts.rs            # Part conversion
├── events.rs           # Event conversion
├── metadata.rs         # Metadata management
├── processor.rs        # Event processor
└── executor.rs         # Executor implementation

adk-server/tests/
├── a2a_parts_tests.rs
├── a2a_events_tests.rs
├── a2a_processor_tests.rs
├── a2a_executor_tests.rs
└── a2a_integration_tests.rs
```

## API Surface

### Public API:
```rust
// Main executor
pub use executor::{Executor, ExecutorConfig};

// Conversions
pub use parts::{adk_parts_to_a2a, a2a_parts_to_adk};
pub use events::{event_to_message, message_to_event};

// Agent card
pub use agent_card::build_agent_skills;

// Metadata
pub use metadata::{InvocationMeta, to_invocation_meta};
```

## Success Criteria

- [ ] All 6 modules implemented
- [ ] Feature parity with Go implementation
- [ ] All unit tests passing
- [ ] Integration tests passing
- [ ] Documentation complete
- [ ] Examples working

## Notes

- Use `a2a-rs` for A2A types (AgentCard, Message, Task, etc.)
- Implement conversion layer between ADK and A2A
- Follow Go's logic exactly for compatibility
- Maintain async/await throughout
- Use proper error handling with `Result<T>`

## Next Steps

1. Start with Phase 1 (Setup & Types)
2. Implement modules in order
3. Test each module before moving to next
4. Integration testing at the end

Ready to begin implementation?
