# adk-action

Shared action node types for ADK-Rust graph workflows.

[![Crates.io](https://img.shields.io/crates/v/adk-action.svg)](https://crates.io/crates/adk-action)
[![Documentation](https://docs.rs/adk-action/badge.svg)](https://docs.rs/adk-action)
[![License](https://img.shields.io/crates/l/adk-action.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

This crate provides the type definitions, error types, and variable interpolation utilities used by both [`adk-studio`](https://github.com/zavora-ai/adk-studio) (visual builder) and `adk-graph` (runtime engine) for deterministic, non-LLM workflow operations.

## Overview

Action nodes are programmatic graph nodes that perform specific operations — HTTP calls, state manipulation, conditional branching, looping, file I/O, notifications, and more. They complement LLM agent nodes in `adk-graph` workflows.

`adk-action` is the shared type layer. It contains no execution logic, no HTTP clients, no database drivers. Execution lives in `adk-graph`'s `ActionNodeExecutor` behind the `action` feature flags.

## Contents

| Module | Description |
|--------|-------------|
| `types` | All 14 action node config structs, `StandardProperties`, and the `ActionNodeConfig` tagged union |
| `error` | `ActionError` enum with 24 variants covering all failure modes |
| `interpolation` | `interpolate_variables()` and `get_nested_value()` for `{{variable}}` template resolution |

## Action Node Types

The last column is the node's status in `adk-graph` 3.0.

| Category | Node | Description | Runs in `adk-graph` |
|----------|------|-------------|---------------------|
| Trigger | `TriggerNodeConfig` | `manual`, `webhook`, `schedule`, and `event` triggers | Yes (`action`; `action-trigger` for the cron/webhook runtime) |
| Data | `HttpNodeConfig` | HTTP requests with auth, interpolation, status validation | Yes (`action-http`) |
| Data | `SetNodeConfig` | `set`, `merge`, and `delete` state variables | Yes |
| Data | `TransformNodeConfig` | `template`, `jsonpath`, and `builtin` transforms with type coercion | Yes |
| Control | `SwitchNodeConfig` | Conditional routing (`first_match` or `all_match`) on a field, operator, and value | Yes |
| Control | `LoopNodeConfig` | `for_each`, `while`, and `times` iteration | Yes |
| Control | `MergeNodeConfig` | Synchronize parallel branches (`wait_all`, `wait_any`, `wait_n`) | Yes |
| Control | `WaitNodeConfig` | `fixed` delays, `until` a timestamp, `condition` polling, `webhook` waits | Yes |
| Compute | `CodeNodeConfig` | Code in `rust`, `javascript`, or `typescript` | `rust` only; JavaScript and TypeScript are rejected when the graph compiles |
| Infra | `DatabaseNodeConfig` | SQL (PostgreSQL/MySQL/SQLite), MongoDB, Redis | No — rejected when the graph compiles |
| Comms | `EmailNodeConfig` | IMAP monitoring and SMTP sending | No — rejected when the graph compiles |
| Comms | `NotificationNodeConfig` | Slack, Discord, Teams, generic webhooks | Yes (`action-http`) |
| Comms | `RssNodeConfig` | RSS/Atom feed fetching with filters and seen-item tracking | Yes (`action-rss`) |
| IO | `FileNodeConfig` | `read`, `write`, `list`, and `delete` files | Yes, confined to allowed roots |

`adk-graph` evaluates the switch operators `eq`, `neq`, `gt`, `lt`, `gte`, `lte`, `contains`,
`startsWith`, `endsWith`, `matches`, `in`, `empty`, and `exists`; the operator is a string in
`ExpressionMode`.

## Installation

```toml
[dependencies]
adk-action = "3.0"
```

## Usage

### Defining an action node

```rust
use adk_action::*;
use serde_json::json;

fn main() {
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
            mapping: InputOutputMapping { input_mapping: None, output_key: "initResult".into() },
        },
        mode: SetMode::Set,
        variables: vec![Variable {
            key: "greeting".into(),
            value: json!("Hello, ADK!"),
            value_type: "string".into(),
            is_secret: false,
        }],
        env_vars: None,
    });

    // Serialize to JSON (same format as adk-studio projects)
    let json = serde_json::to_string_pretty(&config).unwrap();
    assert!(json.contains(r#""type": "set""#));
}
```

### Variable interpolation

```rust
use adk_action::{get_nested_value, interpolate_variables};
use serde_json::json;
use std::collections::HashMap;

fn main() {
    let mut state = HashMap::new();
    state.insert("name".to_string(), json!("Alice"));
    state.insert("user".to_string(), json!({ "email": "alice@example.com" }));

    // Simple interpolation
    let result = interpolate_variables("Hello, {{name}}!", &state);
    assert_eq!(result, "Hello, Alice!");

    // Nested dot-notation
    let email = get_nested_value(&state, "user.email");
    assert_eq!(email, Some(&json!("alice@example.com")));
}
```

### JSON round-trip (adk-studio ↔ adk-graph)

```rust
use adk_action::ActionNodeConfig;

fn main() {
    // Deserialize from adk-studio JSON
    let json = r#"{"type":"set","id":"my_node","name":"Set Vars",
      "errorHandling":{"mode":"stop"},"tracing":{"enabled":true,"logLevel":"debug"},
      "callbacks":{},"execution":{"timeout":30000},
      "mapping":{"outputKey":"result"},"mode":"set","variables":[]}"#;

    let config: ActionNodeConfig = serde_json::from_str(json).unwrap();

    // Re-serializing and parsing again yields an equal config
    let roundtrip = serde_json::to_string(&config).unwrap();
    let reparsed: ActionNodeConfig = serde_json::from_str(&roundtrip).unwrap();
    assert_eq!(config, reparsed);
}
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
| Timing | `ConditionTimeout`, `WebhookTimeout`, `WebhookCancelled`, `InvalidTimestamp` |
| Compute | `CodeExecution`, `SandboxInit`, `Transform` |
| Infrastructure | `MissingCredential`, `NoDatabase` |
| Communication | `EmailAuth`, `EmailSend`, `NotificationSend`, `RssFetch`, `RssParse` |
| File I/O | `FileRead`, `FileWrite`, `FileDelete`, `FileParse` |
| Other | `Other` |

`From<ActionError> for AdkError` maps every variant to an `ErrorCategory` and an `action.*`
error code, such as `action.timeout` (`Timeout`) or `action.missing_credential` (`Unauthorized`).

## Design Principles

- **Lean**: Only types, errors, and interpolation. No runtime, no HTTP clients, no database drivers.
- **Shared**: Identical types used by adk-studio (visual builder) and adk-graph (runtime engine).
- **Serialization-first**: All types derive `Serialize`/`Deserialize` with `camelCase` field naming and `skip_serializing_if` for optional fields.
- **Tagged union**: `ActionNodeConfig` uses `#[serde(tag = "type")]` so the JSON `"type"` field determines the variant.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
