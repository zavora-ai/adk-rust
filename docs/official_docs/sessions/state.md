# State Management

Session state in ADK-Rust allows agents to store and retrieve data that persists across conversation turns. State is organized using key prefixes that determine the scope and lifetime of the data.

## Overview

State is stored as key-value pairs where:
- Keys are strings with optional prefixes
- Values are JSON values (`serde_json::Value`)

The prefix system enables different scoping levels:
- **Session-scoped**: Default, tied to a single session
- **User-scoped**: Shared across all sessions for a user
- **App-scoped**: Shared across all users of an application
- **Temporary**: Cleared after each invocation

## State Trait

The `State` trait defines the interface for state access:

```rust
use serde_json::Value;
use std::collections::HashMap;

pub trait State: Send + Sync {
    /// Get a value by key
    fn get(&self, key: &str) -> Option<Value>;
    
    /// Set a value
    fn set(&mut self, key: String, value: Value);
    
    /// Get all state as a map
    fn all(&self) -> HashMap<String, Value>;
}
```

There's also a `ReadonlyState` trait for read-only access:

```rust
pub trait ReadonlyState: Send + Sync {
    fn get(&self, key: &str) -> Option<Value>;
    fn all(&self) -> HashMap<String, Value>;
}
```

## State Key Prefixes

ADK-Rust uses three key prefixes to control state scoping:

| Prefix | Constant | Scope |
|--------|----------|-------|
| `app:` | `KEY_PREFIX_APP` | Shared across all users and sessions |
| `user:` | `KEY_PREFIX_USER` | Shared across all sessions for a user |
| `temp:` | `KEY_PREFIX_TEMP` | Cleared after each invocation |
| (none) | - | Session-scoped (default) |

### `app:` - Application State

State shared across all users and sessions of an application.

```rust
use adk_session::KEY_PREFIX_APP;

// KEY_PREFIX_APP = "app:"
let key = format!("{}settings", KEY_PREFIX_APP);  // "app:settings"
```

Use cases:
- Application configuration
- Shared resources
- Global counters or statistics

### `user:` - User State

State shared across all sessions for a specific user.

```rust
use adk_session::KEY_PREFIX_USER;

// KEY_PREFIX_USER = "user:"
let key = format!("{}preferences", KEY_PREFIX_USER);  // "user:preferences"
```

Use cases:
- User preferences
- User profile data
- Cross-session user context

### `temp:` - Temporary State

State that is cleared after each invocation. Not persisted.

```rust
use adk_session::KEY_PREFIX_TEMP;

// KEY_PREFIX_TEMP = "temp:"
let key = format!("{}current_step", KEY_PREFIX_TEMP);  // "temp:current_step"
```

Use cases:
- Intermediate computation results
- Current operation context
- Data that shouldn't persist

### No Prefix - Session State

Keys without a prefix are session-scoped (default behavior).

```rust
let key = "conversation_topic";  // Session-scoped
```

Use cases:
- Conversation context
- Session-specific data
- Turn-by-turn state

## Setting Initial State

State can be initialized when creating a session:

```rust
use adk_session::{InMemorySessionService, SessionService, CreateRequest, KEY_PREFIX_APP, KEY_PREFIX_USER};
use serde_json::json;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut initial_state = HashMap::new();

    // App-scoped state
    initial_state.insert(
        format!("{}version", KEY_PREFIX_APP),
        json!("1.0.0")
    );

    // User-scoped state
    initial_state.insert(
        format!("{}name", KEY_PREFIX_USER),
        json!("Alice")
    );

    // Session-scoped state
    initial_state.insert(
        "topic".to_string(),
        json!("Getting started")
    );

    let service = InMemorySessionService::new();
    let session = service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: None,
        state: initial_state,
    }).await?;
    
    Ok(())
}
```

## Reading State

Access state through the session's `state()` method:

```rust
let state = session.state();

// Get a specific key
if let Some(value) = state.get("topic") {
    println!("Topic: {}", value);
}

// Get app-scoped state
if let Some(version) = state.get("app:version") {
    println!("App version: {}", version);
}

// Get all state
let all_state = state.all();
for (key, value) in all_state {
    println!("{}: {}", key, value);
}
```

## State Updates via Events

State is typically updated through event actions. When an event is appended to a session, its `state_delta` is applied:

```rust
use adk_session::{Event, EventActions};
use serde_json::json;
use std::collections::HashMap;

let mut state_delta = HashMap::new();
state_delta.insert("counter".to_string(), json!(42));
state_delta.insert("user:last_seen".to_string(), json!("2024-01-15"));

let mut event = Event::new("invocation_123");
event.actions = EventActions {
    state_delta,
    ..Default::default()
};

// When this event is appended, state is updated
service.append_event(session.id(), event).await?;
```

## State Scoping Behavior

The session service handles state scoping automatically:

### On Session Creation

1. Extract `app:` prefixed keys → Store in app state
2. Extract `user:` prefixed keys → Store in user state
3. Remaining keys (except `temp:`) → Store in session state
4. Merge all scopes for the returned session

### On Session Retrieval

1. Load app state for the application
2. Load user state for the user
3. Load session state
4. Merge all scopes (app → user → session)

### On Event Append

1. Extract state delta from event
2. Filter out `temp:` keys (not persisted)
3. Apply `app:` deltas to app state
4. Apply `user:` deltas to user state
5. Apply remaining deltas to session state

## Complete Example

```rust
use adk_session::{
    InMemorySessionService, SessionService, CreateRequest, GetRequest,
    KEY_PREFIX_APP, KEY_PREFIX_USER,
};
use serde_json::json;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = InMemorySessionService::new();
    
    // Create first session with initial state
    let mut state1 = HashMap::new();
    state1.insert(format!("{}theme", KEY_PREFIX_APP), json!("dark"));
    state1.insert(format!("{}language", KEY_PREFIX_USER), json!("en"));
    state1.insert("context".to_string(), json!("session1"));
    
    let session1 = service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "alice".to_string(),
        session_id: Some("s1".to_string()),
        state: state1,
    }).await?;
    
    // Create second session for same user
    let mut state2 = HashMap::new();
    state2.insert("context".to_string(), json!("session2"));
    
    let session2 = service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "alice".to_string(),
        session_id: Some("s2".to_string()),
        state: state2,
    }).await?;
    
    // Session 2 inherits app and user state
    let s2_state = session2.state();
    
    // App state is shared
    assert_eq!(s2_state.get("app:theme"), Some(json!("dark")));
    
    // User state is shared
    assert_eq!(s2_state.get("user:language"), Some(json!("en")));
    
    // Session state is separate
    assert_eq!(s2_state.get("context"), Some(json!("session2")));
    
    println!("State scoping works correctly!");
    Ok(())
}
```

## Instruction Templating with State

State values can be injected into agent instructions using `{key}` syntax:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("personalized_assistant")
    .instruction("You are helping {user:name} with {topic}. Their preferred language is {user:language}.")
    .model(Arc::new(model))
    .build()?;
```

When the agent runs, `{user:name}`, `{topic}`, and `{user:language}` are replaced with values from session state.

## Best Practices

### 1. Use Appropriate Scopes

```rust
// ✅ Good: User preferences in user scope
"user:theme"
"user:timezone"

// ✅ Good: Session-specific context without prefix
"current_task"
"conversation_summary"

// ✅ Good: App-wide settings in app scope
"app:model_version"
"app:feature_flags"

// ❌ Bad: User data in session scope (lost between sessions)
"user_preferences"  // Should be "user:preferences"
```

### 2. Use Temporary State for Intermediate Data

```rust
// ✅ Good: Intermediate results in temp scope
"temp:search_results"
"temp:current_step"

// ❌ Bad: Intermediate data persisted unnecessarily
"search_results"  // Will be saved to database
```

### 3. Keep State Keys Consistent

```rust
// ✅ Good: Consistent naming convention
"user:preferences.theme"
"user:preferences.language"

// ❌ Bad: Inconsistent naming
"user:theme"
"userLanguage"
"user-timezone"
```

## Related

- [Sessions](sessions.md) - Session management overview
- [Events](../events/events.md) - Event structure and state_delta
- [LlmAgent](../agents/llm-agent.md) - Instruction templating

---

**Previous**: [← Sessions](sessions.md) | **Next**: [Callbacks →](../callbacks/callbacks.md)
