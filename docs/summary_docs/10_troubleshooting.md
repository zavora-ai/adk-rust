# Troubleshooting Guide

Common issues and solutions when working with ADK-Rust.

## Installation Issues

### Rust Version Too Old

**Error**:
```
error: package requires rustc 1.75 or newer
```

**Solution**:
```bash
rustup update
rustc --version  # Verify version
```

### Missing System Dependencies

**Error** (Linux):
```
error: linking with `cc` failed
```

**Solution**:
```bash
# Ubuntu/Debian
sudo apt-get install build-essential pkg-config libssl-dev

# Fedora/RHEL
sudo dnf install gcc pkg-config openssl-devel

# macOS
xcode-select --install
```

## API Key Issues

### API Key Not Found

**Error**:
```
Error: environment variable not found: GOOGLE_API_KEY
```

**Solution**:
```bash
export GOOGLE_API_KEY="your-api-key-here"

# Verify
echo $GOOGLE_API_KEY
```

### Invalid API Key

**Error**:
```
Model error: 401 Unauthorized
```

**Solution**:
1. Verify key is correct
2. Generate new key at [Google AI Studio](https://aistudio.google.com/app/apikey)
3. Check API quotas/limits

### Rate Limit Exceeded

**Error**:
```
Model error: 429 Too Many Requests
```

**Solution**:
- Implement exponential backoff
- Use rate limiting
- Upgrade API tier

```rust
use tokio::time::{sleep, Duration};

for attempt in 0..3 {
    match model.generate_content(&request).await {
        Ok(response) => return Ok(response),
        Err(e) if e.to_string().contains("429") => {
            sleep(Duration::from_secs(2_u64.pow(attempt))).await;
        }
        Err(e) => return Err(e),
    }
}
```

## Runtime Errors

### Event Stream Errors

**Error**:
```
Error: Agent error: failed to process event
```

**Debug**:
```rust
while let Some(event) = events.next().await {
    match event {
        Ok(evt) => {
            println!("Event: {:?}", evt);  // Debug output
            // Process event
        }
        Err(e) => {
            eprintln!("Event error: {:?}", e);  // Full error details
        }
    }
}
```

### Tool Execution Failures

**Error**:
```
Tool error: function call failed
```

**Solutions**:
1. Check tool description matches usage
2. Validate tool arguments
3. Add error handling in tool

```rust
let tool = FunctionTool::new(
    "calculate",
    "Add two numbers: add(a: number, b: number)",
    |_ctx, args| async move {
        // Validate inputs
        let a = args.get("a")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| AdkError::Tool("Missing or invalid 'a'".into()))?;
        
        let b = args.get("b")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| AdkError::Tool("Missing or invalid 'b'".into()))?;
        
        Ok(json!({"result": a + b}))
    }
);
```

### Session Errors

**Error**:
```
Session error: session not found
```

**Solution**:
- Ensure session is created before use
- Check session ID is consistent
- Verify session service is initialized

```rust
// Explicit session creation
let session_service = Arc::new(InMemorySessionService::new());

// Consistent IDs
let user_id = "user123".to_string();
let session_id = "session456".to_string();

let runner = Runner::new("app", agent, session_service);
```

## Performance Issues

### Slow Response Times

**Symptoms**:
- Agents take >10 seconds to respond
- High latency in API calls

**Solutions**:

1. **Use Streaming**:
```rust
// Stream for immediate feedback
let mut events = runner.run(user_id, session_id, content).await?;

while let Some(event) = events.next().await {
    // Display partial results immediately
}
```

2. **Reduce Sequential Depth**:
```rust
// ❌ Too many steps
SequentialAgent::new("slow", vec![s1, s2, s3, s4, s5, s6, s7, s8]);

// ✅ Fewer, focused steps
SequentialAgent::new("fast", vec![s1, s2, s3]);
```

3. **Use Parallel Where Possible**:
```rust
// ✅ Run concurrently
ParallelAgent::new("fast", vec![a1, a2, a3]);
```

4. **Cache Results**:
```rust
use moka::future::Cache;

let cache: Cache<String, String> = Cache::new(1000);

if let Some(result) = cache.get(&key).await {
    return Ok(result);
}

let result = expensive_operation().await?;
cache.insert(key, result.clone()).await;
```

### High Memory Usage

**Symptoms**:
- Process using >1GB RAM
- Out of memory errors

**Solutions**:

1. **Limit Context Length**:
```rust
let config = GenerateContentConfig {
    max_output_tokens: Some(2048),  // Limit output
    ..Default::default()
};

let model = GeminiModel::with_config(&api_key, "gemini-2.0-flash-exp", config)?;
```

2. **Clear Session History**:
```rust
// Periodically clear old sessions
session_service.delete(DeleteRequest {
    user_id: user_id.clone(),
    session_id: old_session_id,
}).await?;
```

3. **Use Database Sessions**:
```rust
// Store sessions on disk instead of memory
let session_service = DatabaseSessionService::new("sqlite://sessions.db").await?;
```

## Compilation Issues

### Trait Not Satisfied

**Error**:
```
error: the trait bound `MyType: Send` is not satisfied
```

**Solution**:
Ensure all custom types implement `Send + Sync`:

```rust
struct MyAgent {
    // All fields must be Send + Sync
    data: Arc<SomeData>,  // Use Arc for shared data
}

unsafe impl Send for MyAgent {}
unsafe impl Sync for MyAgent {}
```

### Lifetime Errors

**Error**:
```
error: cannot return value referencing local variable
```

**Solution**:
Use owned types or `Arc`:

```rust
// ❌ Returns reference to local data
fn bad() -> &str {
    let s = String::from("hello");
    &s  // Error: s is dropped
}

// ✅ Returns owned data
fn good() -> String {
    String::from("hello")
}

// ✅ Use Arc for shared ownership
fn shared() -> Arc<String> {
    Arc::new(String::from("hello"))
}
```

## Database Issues

### Database Locked (SQLite)

**Error**:
```
Session error: database is locked
```

**Solutions**:

1. **Use WAL Mode**:
```sql
PRAGMA journal_mode=WAL;
```

2. **Increase Timeout**:
```rust
let session_service = DatabaseSessionService::new(
    "sqlite://sessions.db?busy_timeout=5000"
).await?;
```

3. **Switch to PostgreSQL** for high concurrency:
```rust
let session_service = DatabaseSessionService::new(
    "postgresql://user:pass@localhost/adk"
).await?;
```

### Migration Errors

**Error**:
```
Migration error: table already exists
```

**Solution**:
- Drop database and recreate
- Run migrations manually
- Use migration tools

```bash
# Reset database
rm sessions.db

# Or use sqlx
sqlx database reset
```

## MCP Issues

### MCP Server Won't Start

**Error**:
```
Tool error: failed to start MCP server
```

**Debug**:
```bash
# Test MCP server manually
npx -y @modelcontextprotocol/server-filesystem /tmp

# Check output for errors
```

**Solutions**:
- Verify `npx` is installed
- Check server package exists
- Ensure arguments are correct

### MCP Tool Not Found

**Error**:
```
Tool error: tool 'read_file' not found
```

**Debug**:
```rust
let tools = mcp_toolset.tools();
for tool in tools {
    println!("Available tool: {}", tool.name());
}
```

**Solution**:
Ensure MCP server exposes the expected tools.

## Networking Issues

### Server Won't Bind

**Error**:
```
Server error: Address already in use
```

**Solution**:
```bash
# Find process using port
lsof -i :8080
# or
netstat -tuln | grep 8080

# Kill process
kill <PID>

# Or use different port
adk-cli serve --port 8081
```

### Connection Refused

**Error**:
```
Error: Connection refused
```

**Solutions**:
- Check server is running
- Verify correct host/port
- Check firewall settings

```bash
# Test server is reachable
curl http://localhost:8080/health

# Check firewall (Linux)
sudo ufw status
sudo ufw allow 8080
```

## Debugging Techniques

### Enable Debug Logging

```bash
export RUST_LOG=debug
# or more specific
export RUST_LOG=adk_runner=debug,adk_agent=trace

cargo run
```

### Add Trace Statements

```rust
use tracing::{debug, info, warn};

debug!("Processing input: {}", input);
info!("Agent started: {}", agent.name());
warn!("Slow operation: {}ms", duration);
```

### Inspect Events

```rust
while let Some(event) = events.next().await {
    match event {
        Ok(evt) => {
            // Log full event
            eprintln!("Event: {:#?}", evt);
            
            // Check specific fields
            if let Some(content) = &evt.content {
                eprintln!("Content: {:?}", content);
            }
            if !evt.actions.tool_calls.is_empty() {
                eprintln!("Tool calls: {:?}", evt.actions.tool_calls);
            }
        }
        Err(e) => {
            eprintln!("Error: {:?}", e);
        }
    }
}
```

### Use MockLlm for Testing

```rust
use adk_model::MockLlm;

let mock = MockLlm::new("Mocked response");

let agent = LlmAgentBuilder::new("test")
    .model(Arc::new(mock))
    .build()?;

// Test without real API calls
```

## Getting Help

If you're still stuck:

1. **Check Examples**: Review working examples in `examples/`
2. **Read Documentation**: See [docs/](.)
3. **Search Issues**: Check GitHub issues
4. **Enable Debug Logging**: Use `RUST_LOG=debug`
5. **Create Minimal Reproduction**: Isolate the problem
6. **Ask for Help**: Open a GitHub issue with:
   - Rust version
   - ADK-Rust version
   - Minimal code example
   - Full error message
   - Log output

---

**Previous**: [CLI Usage](09_cli.md) | **Next**: [FAQ](11_faq.md)
