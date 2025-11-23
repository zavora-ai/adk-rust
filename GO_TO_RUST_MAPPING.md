# Go to Rust Mapping Guide

This document provides a quick reference for converting Go ADK concepts to Rust ADK.

## Language Constructs

| Go | Rust | Notes |
|----|------|-------|
| `interface{}` | `dyn Trait` | Use trait objects with `Arc<dyn Trait>` |
| `interface Agent` | `trait Agent` | Traits define behavior |
| `type Agent interface { ... }` | `pub trait Agent { ... }` | Public traits |
| `func (a *Agent) Method()` | `impl Agent for MyAgent { fn method(&self) }` | Method implementation |
| `goroutine` | `tokio::spawn` | Async task spawning |
| `channel` | `tokio::sync::mpsc` | Message passing |
| `context.Context` | `Arc<Context>` | Shared context |
| `sync.Mutex` | `tokio::sync::Mutex` | Async mutex |
| `sync.RWMutex` | `tokio::sync::RwLock` | Async read-write lock |
| `error` | `Result<T, AdkError>` | Explicit error handling |
| `nil` | `None` or `null` | Use `Option<T>` |
| `make(map[K]V)` | `HashMap::new()` | Hash maps |
| `make([]T, n)` | `Vec::with_capacity(n)` | Vectors |
| `append(slice, item)` | `vec.push(item)` | Append to vector |
| `for range` | `for item in iter` | Iteration |
| `defer` | `Drop` trait | Cleanup on scope exit |

## ADK-Specific Mappings

### Agent Interface

**Go:**
```go
type Agent interface {
    Name() string
    Description() string
    Run(InvocationContext) iter.Seq2[*session.Event, error]
    SubAgents() []Agent
}
```

**Rust:**
```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn sub_agents(&self) -> &[Arc<dyn Agent>];
    async fn run(&self, ctx: Arc<InvocationContext>) 
        -> Result<EventStream>;
}
```

### LLM Agent Creation

**Go:**
```go
agent, err := llmagent.New(llmagent.Config{
    Name:        "my_agent",
    Model:       model,
    Description: "My agent",
    Tools:       []tool.Tool{googleSearch},
})
```

**Rust:**
```rust
let agent = LlmAgentBuilder::new("my_agent")
    .model(Arc::new(model))
    .description("My agent")
    .tool(Arc::new(google_search))
    .build()?;
```

### Tool Interface

**Go:**
```go
type Tool interface {
    Name() string
    Description() string
    IsLongRunning() bool
}
```

**Rust:**
```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn is_long_running(&self) -> bool { false }
    async fn execute(&self, ctx: Arc<ToolContext>, args: Value) 
        -> Result<Value>;
}
```

### Function Tool

**Go:**
```go
tool, err := functiontool.New(
    functiontool.Config{
        Name:        "get_weather",
        Description: "Gets weather",
    },
    func(ctx tool.Context, args WeatherArgs) (WeatherResult, error) {
        // implementation
    },
)
```

**Rust:**
```rust
let tool = FunctionTool::new(
    "get_weather".to_string(),
    "Gets weather".to_string(),
    |ctx: Arc<ToolContext>, args: WeatherArgs| {
        Box::pin(async move {
            // implementation
            Ok(result)
        })
    },
);
```

### Session Service

**Go:**
```go
type Service interface {
    Get(ctx context.Context, req *GetRequest) (*GetResponse, error)
    AppendEvent(ctx context.Context, session Session, event *Event) error
}
```

**Rust:**
```rust
#[async_trait]
pub trait SessionService: Send + Sync {
    async fn get(&self, req: &GetRequest) -> Result<Session>;
    async fn append_event(&self, session: &Session, event: Event) 
        -> Result<()>;
}
```

### Runner

**Go:**
```go
runner, err := runner.New(runner.Config{
    Agent:          agent,
    SessionService: sessionService,
})

for event, err := range runner.Run(ctx, userID, sessionID, msg, cfg) {
    if err != nil {
        // handle error
    }
    // process event
}
```

**Rust:**
```rust
let runner = Runner::new(RunnerConfig {
    agent: Arc::new(agent),
    session_service: Arc::new(session_service),
    ..Default::default()
})?;

let mut events = runner.run(ctx, user_id, session_id, msg, cfg).await?;
while let Some(event) = events.next().await {
    match event {
        Ok(event) => {
            // process event
        }
        Err(e) => {
            // handle error
        }
    }
}
```

### Event Streaming

**Go:**
```go
func (a *Agent) Run(ctx InvocationContext) iter.Seq2[*Event, error] {
    return func(yield func(*Event, error) bool) {
        event := &Event{...}
        if !yield(event, nil) {
            return
        }
    }
}
```

**Rust:**
```rust
async fn run(&self, ctx: Arc<InvocationContext>) -> Result<EventStream> {
    let stream = async_stream::stream! {
        let event = Event { ... };
        yield Ok(event);
    };
    Ok(Box::pin(stream))
}
```

### Error Handling

**Go:**
```go
result, err := someFunction()
if err != nil {
    return nil, fmt.Errorf("operation failed: %w", err)
}
```

**Rust:**
```rust
let result = some_function()
    .map_err(|e| AdkError::Operation(format!("operation failed: {}", e)))?;
```

### Context Management

**Go:**
```go
type InvocationContext interface {
    Session() Session
    Agent() Agent
    Artifacts() Artifacts
    Memory() Memory
}
```

**Rust:**
```rust
pub struct InvocationContext {
    session: Arc<RwLock<dyn MutableSession>>,
    agent: Arc<dyn Agent>,
    artifacts: Option<Arc<dyn ArtifactService>>,
    memory: Option<Arc<dyn MemoryService>>,
}

impl InvocationContext {
    pub fn session(&self) -> Arc<RwLock<dyn MutableSession>> {
        Arc::clone(&self.session)
    }
    // ... other methods
}
```

## Async/Concurrency Patterns

### Spawning Tasks

**Go:**
```go
go func() {
    // async work
}()
```

**Rust:**
```rust
tokio::spawn(async move {
    // async work
});
```

### Channels

**Go:**
```go
ch := make(chan Event)
go func() {
    ch <- event
}()
event := <-ch
```

**Rust:**
```rust
let (tx, mut rx) = tokio::sync::mpsc::channel(100);
tokio::spawn(async move {
    tx.send(event).await.unwrap();
});
let event = rx.recv().await;
```

### Select/Concurrent Operations

**Go:**
```go
select {
case event := <-ch1:
    // handle event
case <-ctx.Done():
    // handle cancellation
}
```

**Rust:**
```rust
tokio::select! {
    event = rx.recv() => {
        // handle event
    }
    _ = cancellation_token.cancelled() => {
        // handle cancellation
    }
}
```

### Parallel Execution

**Go:**
```go
var wg sync.WaitGroup
for _, agent := range agents {
    wg.Add(1)
    go func(a Agent) {
        defer wg.Done()
        a.Run(ctx)
    }(agent)
}
wg.Wait()
```

**Rust:**
```rust
let handles: Vec<_> = agents.iter()
    .map(|agent| {
        let agent = Arc::clone(agent);
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            agent.run(ctx).await
        })
    })
    .collect();

for handle in handles {
    handle.await??;
}
```

## Memory Management

### Shared Ownership

**Go:**
```go
// Garbage collected, automatic
agent := &Agent{...}
subAgent := agent  // Both point to same object
```

**Rust:**
```rust
// Explicit reference counting
let agent = Arc::new(Agent { ... });
let sub_agent = Arc::clone(&agent);  // Increment ref count
```

### Mutable Shared State

**Go:**
```go
type State struct {
    mu sync.RWMutex
    data map[string]any
}

func (s *State) Get(key string) any {
    s.mu.RLock()
    defer s.mu.RUnlock()
    return s.data[key]
}
```

**Rust:**
```rust
pub struct State {
    data: Arc<RwLock<HashMap<String, Value>>>,
}

impl State {
    pub async fn get(&self, key: &str) -> Option<Value> {
        let data = self.data.read().await;
        data.get(key).cloned()
    }
}
```

## Type Conversions

### String Handling

**Go:**
```go
name := "agent"
description := fmt.Sprintf("Agent: %s", name)
```

**Rust:**
```rust
let name = "agent";
let description = format!("Agent: {}", name);
// Or use String::from() for owned strings
let owned_name = String::from("agent");
```

### JSON Serialization

**Go:**
```go
import "encoding/json"

data, err := json.Marshal(obj)
err = json.Unmarshal(data, &obj)
```

**Rust:**
```rust
use serde_json;

let data = serde_json::to_string(&obj)?;
let obj: MyType = serde_json::from_str(&data)?;
```

### Optional Values

**Go:**
```go
var value *string  // nil if not set
if value != nil {
    // use *value
}
```

**Rust:**
```rust
let value: Option<String> = None;
if let Some(v) = value {
    // use v
}
// Or use match
match value {
    Some(v) => { /* use v */ }
    None => { /* handle absence */ }
}
```

## Testing

### Unit Tests

**Go:**
```go
func TestAgent(t *testing.T) {
    agent := NewAgent()
    result := agent.Run(ctx)
    if result != expected {
        t.Errorf("got %v, want %v", result, expected)
    }
}
```

**Rust:**
```rust
#[tokio::test]
async fn test_agent() {
    let agent = Agent::new();
    let result = agent.run(ctx).await.unwrap();
    assert_eq!(result, expected);
}
```

### Mocking

**Go:**
```go
type MockModel struct {
    GenerateContentFunc func(...) (...)
}

func (m *MockModel) GenerateContent(...) (...) {
    return m.GenerateContentFunc(...)
}
```

**Rust:**
```rust
use mockall::*;

#[automock]
trait Model {
    async fn generate_content(&self, ...) -> Result<...>;
}

#[tokio::test]
async fn test_with_mock() {
    let mut mock = MockModel::new();
    mock.expect_generate_content()
        .returning(|_| Ok(...));
}
```

## Common Pitfalls

### 1. Forgetting Send + Sync
**Problem**: Trait objects used across threads must be `Send + Sync`  
**Solution**: Add bounds: `trait Agent: Send + Sync { ... }`

### 2. Cloning Large Structures
**Problem**: Rust doesn't have GC, cloning is explicit  
**Solution**: Use `Arc` for shared ownership, avoid unnecessary clones

### 3. Blocking in Async Context
**Problem**: Blocking operations in async functions  
**Solution**: Use `tokio::task::spawn_blocking` for blocking code

### 4. Lifetime Issues
**Problem**: Rust requires explicit lifetimes  
**Solution**: Use owned types (`String` vs `&str`) or `Arc` for shared data

### 5. Error Propagation
**Problem**: Go returns `(result, error)`, Rust uses `Result<T, E>`  
**Solution**: Use `?` operator for error propagation

## Best Practices

1. **Use Arc for shared ownership**: Avoid cloning large structures
2. **Prefer async/await**: Use Tokio for all I/O operations
3. **Handle errors explicitly**: Use `Result` and `?` operator
4. **Use builder pattern**: For complex configurations
5. **Document with rustdoc**: Use `///` for public APIs
6. **Write tests**: Use `#[tokio::test]` for async tests
7. **Avoid unsafe**: Only use when absolutely necessary
8. **Use type aliases**: For complex types like `Arc<dyn Trait>`
9. **Implement traits**: For extensibility
10. **Use streams**: For event streaming instead of channels
