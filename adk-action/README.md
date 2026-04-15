# adk-action

Shared action node types for ADK-Rust graph workflows.

This crate provides the type definitions, error types, and variable interpolation utilities used by both `adk-studio` (visual builder) and `adk-graph` (runtime engine) for deterministic, non-LLM workflow operations.

## Overview

Action nodes are programmatic graph nodes that perform specific operations — HTTP calls, state manipulation, conditional branching, looping, file I/O, database queries, notifications, and more. They complement LLM agent nodes in `adk-graph` workflows.

`adk-action` is the shared type layer. It contains no execution logic, no HTTP clients, no database drivers. Execution lives in `adk-graph` behind feature flags.

## Contents

| Module | Description |
|--------|-------------|
| `types` | All 14 action node config structs, `StandardProperties`, and the `ActionNodeConfig` tagged union |
| `error` | `ActionError` enum with 24 variants covering all failure modes |
| `interpolation` | `interpolate_variables()` and `get_nested_value()` for `{{variable}}` template resolution |

## Action Node Types

| Category | Node | Description |
|----------|------|-------------|
| Trigger | `TriggerNodeConfig` | Manual, webhook, schedule, and event triggers |
| Data | `HttpNodeConfig` | HTTP requests with auth, interpolation, status validation |
| Data | `SetNodeConfig` | Set, merge, and delete state variables |
| Data | `TransformNodeConfig` | Template interpolation, JSONPath extraction, type coercion |
| Control | `SwitchNodeConfig` | Conditional routing with 12 typed operators |
| Control | `LoopNodeConfig` | forEach, while, and times iteration |
| Control | `MergeNodeConfig` | Synchronize parallel branches (waitAll/waitAny/waitN) |
| Control | `WaitNodeConfig` | Fixed delays, condition polling, timestamp waits |
| Compute | `CodeNodeConfig` | Execute Rust or sandboxed JavaScript code |
| Infra | `DatabaseNodeConfig` | SQL (PostgreSQL/MySQL/SQLite), MongoDB, Redis |
| Comms | `EmailNodeConfig` | IMAP monitoring and SMTP sending |
| Comms | `NotificationNodeConfig` | Slack, Discord, Teams, generic webhooks |
| Comms | `RssNodeConfig` | RSS/Atom feed fetching with filters and seen-item tracking |
| IO | `FileNodeConfig` | Read, write, list, and delete files |

## Installation

```toml
[dependencies]
adk-action = "0.6.0"
```

## Usage

### Defining an action node

```rust
use adk_action::*;
use serde_json::json;

let config = ActionNodeConfig::Set(SetNodeConfig {
    standard: StandardProperties {
        id: "init_vars".into(),
        name: "Initialize Variables".into(),
        description: None,
        position: None,
        error_handling: ErrorHandling {
            mode: ErrorMode::Stop,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        },
        tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
        callbacks: Callbacks::default(),
        execution: ExecutionControl { timeout: 30000, condition: None },
        mapping: InputOutputMapping {
            input_mapping: None,
            output_key: "initResult".into(),
        },
    },
    mode: SetMode::Set,
    variables: vec![
        Variable {
            key: "greeting".into(),
            value: json!("Hello, ADK!"),
            value_type: "string".into(),
            is_secret: false,
        },
    ],
    env_vars: None,
});

// Serialize to JSON (same format as adk-studio projects)
let json = serde_json::to_string_pretty(&config).unwrap();
```

### Variable interpolation

```rust
use adk_action::interpolate_variables;
use std::collections::HashMap;
use serde_json::json;

let mut state = HashMap::new();
state.insert("name".to_string(), json!("Alice"));
state.insert("user".to_string(), json!({"email": "alice@example.com"}));

// Simple interpolation
let result = interpolate_variables("Hello, {{name}}!", &state);
assert_eq!(result, "Hello, Alice!");

// Nested dot-notation
let email = adk_action::get_nested_value(&state, "user.email");
assert_eq!(email, Some(&json!("alice@example.com")));
```

### JSON round-trip (adk-studio ↔ adk-graph)

```rust
use adk_action::ActionNodeConfig;

// Deserialize from adk-studio JSON
let json = r#"{"type":"set","id":"my_node","name":"Set Vars",
  "errorHandling":{"mode":"stop"},"tracing":{"enabled":true,"logLevel":"debug"},
  "callbacks":{},"execution":{"timeout":30000},
  "mapping":{"outputKey":"result"},"mode":"set","variables":[]}"#;

let config: ActionNodeConfig = serde_json::from_str(json).unwrap();

// Re-serialize produces identical JSON
let roundtrip = serde_json::to_string(&config).unwrap();
let reparsed: ActionNodeConfig = serde_json::from_str(&roundtrip).unwrap();
assert_eq!(config, reparsed);
```

## StandardProperties

Every action node includes `StandardProperties` with:

- **Identity**: `id`, `name`, `description`
- **Error handling**: `mode` (stop/continue/retry/fallback), `retry_count`, `retry_delay`, `fallback_value`
- **Tracing**: `enabled`, `log_level` (none/error/info/debug)
- **Callbacks**: `on_start`, `on_complete`, `on_error`
- **Execution control**: `timeout` (ms), `condition` (skip when false)
- **Mapping**: `input_mapping`, `output_key`

## Error Types

`ActionError` covers all action node failure modes with structured variants:

| Category | Variants |
|----------|----------|
| HTTP | `HttpStatus`, `Timeout` |
| Control flow | `NoMatchingBranch`, `NoBranchCompleted`, `InsufficientBranches` |
| Timing | `ConditionTimeout`, `WebhookTimeout`, `InvalidTimestamp` |
| Compute | `CodeExecution`, `SandboxInit`, `Transform` |
| Infrastructure | `MissingCredential`, `NoDatabase` |
| Communication | `EmailAuth`, `EmailSend`, `NotificationSend`, `RssFetch`, `RssParse` |
| File I/O | `FileRead`, `FileWrite`, `FileDelete`, `FileParse` |

All variants implement `From<ActionError> for AdkError` for seamless integration with the ADK error system.

## Design Principles

- **Lean**: Only types, errors, and interpolation. No runtime, no HTTP clients, no database drivers.
- **Shared**: Identical types used by adk-studio (visual builder) and adk-graph (runtime engine).
- **Serialization-first**: All types derive `Serialize`/`Deserialize` with `camelCase` field naming and `skip_serializing_if` for optional fields.
- **Tagged union**: `ActionNodeConfig` uses `#[serde(tag = "type")]` so the JSON `"type"` field determines the variant.

## License

Apache-2.0
