# adk-session

Session management and state persistence for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-session.svg)](https://crates.io/crates/adk-session)
[![Documentation](https://docs.rs/adk-session/badge.svg)](https://docs.rs/adk-session)
[![License](https://img.shields.io/crates/l/adk-session.svg)](LICENSE)

## Overview

`adk-session` provides session and state management for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **InMemorySessionService** - Simple in-memory session storage
- **State Management** - Key-value state with typed prefixes
- **Event History** - Conversation history tracking
- **Session Lifecycle** - Create, update, and restore sessions

## Installation

```toml
[dependencies]
adk-session = "0.3.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.3.0", features = ["sessions"] }
```

## Quick Start

```rust
use adk_session::{InMemorySessionService, SessionService, CreateRequest, KEY_PREFIX_USER};
use serde_json::json;
use std::collections::HashMap;

// Create session service
let service = InMemorySessionService::new();

// Create a new session with initial state
let mut initial_state = HashMap::new();
initial_state.insert(format!("{}name", KEY_PREFIX_USER), json!("Alice"));

let session = service.create(CreateRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: None,  // Auto-generate
    state: initial_state,
}).await?;

// Read state (immutable)
let name = session.state().get("user:name");
```

## State Prefixes

ADK uses prefixes to organize state:

| Prefix | Purpose | Persistence |
|--------|---------|-------------|
| `user:` | User preferences | Across sessions |
| `app:` | Application state | Application-wide |
| `temp:` | Temporary data | Current turn only |

```rust
// State is set at session creation or via CreateRequest
let mut state = HashMap::new();
state.insert("user:theme".to_string(), json!("dark"));
state.insert("temp:current_step".to_string(), json!("2"));

// Read state
let theme = session.state().get("user:theme");
```

## Features

- Thread-safe with async/await
- Automatic event history management
- Pluggable storage backends
- Optional SQLite persistence (`database` feature)
- Optional Vertex AI Session Service backend (`vertex-session` feature)

## Feature Flags

```toml
[dependencies]
adk-session = { version = "0.3.0", features = ["database"] }
```

- `database` - Enable SQLite-backed sessions
- `vertex-session` - Enable Vertex AI Session Service backend (`VertexAiSessionService`)

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Session` trait
- [adk-runner](https://crates.io/crates/adk-runner) - Uses sessions for execution

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
