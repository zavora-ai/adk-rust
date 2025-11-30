# adk-session

Session management and state persistence for ADK agents.

[![Crates.io](https://img.shields.io/crates/v/adk-session.svg)](https://crates.io/crates/adk-session)
[![Documentation](https://docs.rs/adk-session/badge.svg)](https://docs.rs/adk-session)
[![License](https://img.shields.io/crates/l/adk-session.svg)](LICENSE)

## Overview

`adk-session` provides session and state management for ADK agents:

- **InMemorySessionService** - Simple in-memory session storage
- **State Management** - Key-value state with typed prefixes
- **Event History** - Conversation history tracking
- **Session Lifecycle** - Create, update, and restore sessions

## Installation

```toml
[dependencies]
adk-session = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.1", features = ["sessions"] }
```

## Quick Start

```rust
use adk_session::InMemorySessionService;
use adk_core::Session;

// Create session service
let service = InMemorySessionService::new();

// Create a new session
let session = service.create_session("app_name", "user_123").await?;

// Access state
session.state().set("user:name", "Alice".into());
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
// User state persists
session.state().set("user:theme", "dark".into());

// Temp state cleared each turn
session.state().set("temp:current_step", "2".into());
```

## Features

- Thread-safe with async/await
- Automatic event history management
- Pluggable storage backends
- Optional SQLite persistence (`database` feature)

## Feature Flags

```toml
[dependencies]
adk-session = { version = "0.1", features = ["database"] }
```

- `database` - Enable SQLite-backed sessions

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Session` trait
- [adk-runner](https://crates.io/crates/adk-runner) - Uses sessions for execution

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
