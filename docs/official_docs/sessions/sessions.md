# Sessions

Sessions in ADK-Rust provide conversation context management, allowing agents to maintain state across multiple interactions. Sessions store conversation history (events) and arbitrary state data that persists throughout a conversation.

## Overview

A session represents a single conversation between a user and an agent. Each session:

- Has a unique identifier
- Belongs to an application (`app_name`) and user (`user_id`)
- Contains a list of events (conversation history)
- Maintains state data (key-value pairs)
- Tracks the last update time

## Session Trait

The `Session` trait defines the interface for session objects:

```rust
use adk_session::{Events, State};
use chrono::{DateTime, Utc};

pub trait Session: Send + Sync {
    /// Unique session identifier
    fn id(&self) -> &str;
    
    /// Application name this session belongs to
    fn app_name(&self) -> &str;
    
    /// User identifier
    fn user_id(&self) -> &str;
    
    /// Access session state
    fn state(&self) -> &dyn State;
    
    /// Access conversation events
    fn events(&self) -> &dyn Events;
    
    /// Last time the session was updated
    fn last_update_time(&self) -> DateTime<Utc>;
}
```

## SessionService Trait

The `SessionService` trait defines operations for managing sessions:

```rust
use adk_session::{CreateRequest, GetRequest, ListRequest, DeleteRequest, Event, Session};
use adk_core::Result;
use async_trait::async_trait;

#[async_trait]
pub trait SessionService: Send + Sync {
    /// Create a new session
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>>;
    
    /// Retrieve an existing session
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>>;
    
    /// List all sessions for an app/user
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>>;
    
    /// Delete a session
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
    
    /// Append an event to a session
    async fn append_event(&self, session_id: &str, event: Event) -> Result<()>;
}
```

## Request Types

### CreateRequest

```rust
use adk_session::CreateRequest;
use std::collections::HashMap;

let request = CreateRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: None,  // Auto-generate UUID if None
    state: HashMap::new(),  // Initial state
};
```

### GetRequest

```rust
use adk_session::GetRequest;

let request = GetRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_abc".to_string(),
    num_recent_events: Some(10),  // Limit events returned
    after: None,  // Filter events after timestamp
};
```

### ListRequest

```rust
use adk_session::ListRequest;

let request = ListRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
};
```

### DeleteRequest

```rust
use adk_session::DeleteRequest;

let request = DeleteRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_abc".to_string(),
};
```

## SessionService Implementations

ADK-Rust provides two session service implementations:

### InMemorySessionService

Stores sessions in memory. Ideal for development, testing, and single-instance deployments.

```rust
use adk_session::{InMemorySessionService, SessionService, CreateRequest};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create the service
    let session_service = InMemorySessionService::new();
    
    // Create a session
    let session = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: None,
        state: HashMap::new(),
    }).await?;
    
    println!("Session ID: {}", session.id());
    println!("App: {}", session.app_name());
    println!("User: {}", session.user_id());
    
    Ok(())
}
```

### DatabaseSessionService

Stores sessions in a SQLite database. Suitable for production deployments requiring persistence.

```rust
use adk_session::{DatabaseSessionService, SessionService, CreateRequest};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Connect to database
    let session_service = DatabaseSessionService::new("sqlite:sessions.db").await?;
    
    // Run migrations to create tables
    session_service.migrate().await?;
    
    // Create a session
    let session = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: None,
        state: HashMap::new(),
    }).await?;
    
    println!("Session persisted: {}", session.id());
    
    Ok(())
}
```

> **Note**: The `DatabaseSessionService` requires the `database` feature flag:
> ```toml
> adk-session = { version = "0.1", features = ["database"] }
> ```

## Session Lifecycle

### 1. Creation

Sessions are created with a `CreateRequest`. If no `session_id` is provided, a UUID is generated automatically.

```rust
use adk_session::{InMemorySessionService, SessionService, CreateRequest};
use std::collections::HashMap;

let service = InMemorySessionService::new();

// Create with auto-generated ID
let session = service.create(CreateRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: None,
    state: HashMap::new(),
}).await?;

// Create with specific ID
let session = service.create(CreateRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: Some("my-custom-id".to_string()),
    state: HashMap::new(),
}).await?;
```

### 2. Retrieval

Retrieve a session by its identifiers:

```rust
use adk_session::GetRequest;

let session = service.get(GetRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_abc".to_string(),
    num_recent_events: None,
    after: None,
}).await?;

println!("Retrieved session: {}", session.id());
println!("Events: {}", session.events().len());
```

### 3. Event Appending

Events are appended to sessions as the conversation progresses. This is typically handled by the Runner, but can be done manually:

```rust
use adk_session::Event;

let event = Event::new("invocation_123");
service.append_event(session.id(), event).await?;
```

### 4. Listing

List all sessions for a user:

```rust
use adk_session::ListRequest;

let sessions = service.list(ListRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
}).await?;

for session in sessions {
    println!("Session: {} (updated: {})", 
        session.id(), 
        session.last_update_time()
    );
}
```

### 5. Deletion

Delete a session when it's no longer needed:

```rust
use adk_session::DeleteRequest;

service.delete(DeleteRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_abc".to_string(),
}).await?;
```

## Using Sessions with Runner

Sessions are typically managed by the `Runner` when executing agents. The Runner:

1. Creates or retrieves a session
2. Passes session context to the agent
3. Appends events as the conversation progresses
4. Updates session state based on agent actions

```rust
use adk_rust::prelude::*;
use adk_runner::{Runner, RunnerConfig};
use adk_session::InMemorySessionService;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    let agent = LlmAgentBuilder::new("assistant")
        .model(model)
        .instruction("You are a helpful assistant.")
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());

    // Create runner with session service
    let runner = Runner::new(RunnerConfig {
        app_name: "my_app".to_string(),
        agent: Arc::new(agent),
        session_service,
        artifact_service: None,
        memory_service: None,
        run_config: None,
    })?;

    // Run with user and session IDs
    let user_content = Content::new("user").with_text("Hello!");
    let stream = runner.run(
        "user_123".to_string(),
        "session_abc".to_string(),
        user_content,
    ).await?;

    Ok(())
}
```

## Events

The `Events` trait provides access to conversation history:

```rust
pub trait Events: Send + Sync {
    /// Get all events
    fn all(&self) -> Vec<Event>;
    
    /// Get number of events
    fn len(&self) -> usize;
    
    /// Get event at index
    fn at(&self, index: usize) -> Option<&Event>;
    
    /// Check if empty
    fn is_empty(&self) -> bool;
}
```

Access events from a session:

```rust
let events = session.events();
println!("Total events: {}", events.len());

for event in events.all() {
    println!("Event {} by {} at {}", 
        event.id, 
        event.author, 
        event.timestamp
    );
}
```

## Complete Example

```rust
use adk_session::{
    InMemorySessionService, SessionService, 
    CreateRequest, GetRequest, ListRequest, DeleteRequest,
    Event, KEY_PREFIX_USER,
};
use serde_json::json;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = InMemorySessionService::new();
    
    // Create session with initial state
    let mut initial_state = HashMap::new();
    initial_state.insert(format!("{}name", KEY_PREFIX_USER), json!("Alice"));
    initial_state.insert("topic".to_string(), json!("Getting started"));
    
    let session = service.create(CreateRequest {
        app_name: "demo".to_string(),
        user_id: "alice".to_string(),
        session_id: None,
        state: initial_state,
    }).await?;
    
    println!("Created session: {}", session.id());
    
    // Check state
    let state = session.state();
    println!("User name: {:?}", state.get("user:name"));
    println!("Topic: {:?}", state.get("topic"));
    
    // Append an event
    let event = Event::new("inv_001");
    service.append_event(session.id(), event).await?;
    
    // Retrieve session with events
    let session = service.get(GetRequest {
        app_name: "demo".to_string(),
        user_id: "alice".to_string(),
        session_id: session.id().to_string(),
        num_recent_events: None,
        after: None,
    }).await?;
    
    println!("Events: {}", session.events().len());
    
    // List all sessions
    let sessions = service.list(ListRequest {
        app_name: "demo".to_string(),
        user_id: "alice".to_string(),
    }).await?;
    
    println!("Total sessions: {}", sessions.len());
    
    // Delete session
    service.delete(DeleteRequest {
        app_name: "demo".to_string(),
        user_id: "alice".to_string(),
        session_id: session.id().to_string(),
    }).await?;
    
    println!("Session deleted");
    
    Ok(())
}
```

## Related

- [State Management](state.md) - Managing session state with prefixes
- [Events](../events/events.md) - Event structure and actions
- [Runner](../deployment/launcher.md) - Agent execution with sessions

---

**Previous**: [← MCP Tools](../tools/mcp-tools.md) | **Next**: [State Management →](state.md)
