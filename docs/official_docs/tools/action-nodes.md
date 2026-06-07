# Action Nodes

The `adk-action` crate defines 14 action node types used in graph-based workflows. Each node type represents a discrete operation (HTTP call, data transform, conditional branch, etc.) that can be composed into directed graphs via `adk-graph`'s `ActionNodeExecutor`.

## Overview

Action nodes are the building blocks of visual and programmatic workflow graphs. They provide:

- **Typed operations** — 14 node types covering common workflow patterns
- **StandardProperties** — Shared configuration for error handling, tracing, and data mapping
- **Variable interpolation** — `{{variable}}` syntax for dynamic values
- **Graph integration** — Executed by `adk-graph`'s `ActionNodeExecutor`

## Installation

```toml
[dependencies]
adk-action = "1.0.0"

# Or specific action features via umbrella crate
adk-rust = { version = "1.0.0", features = ["action"] }
```

## Node Types (14)

| Node | Purpose | Category |
|------|---------|----------|
| `Trigger` | Entry point — starts workflow execution | Control |
| `HTTP` | Make HTTP requests (GET, POST, PUT, DELETE, PATCH) | I/O |
| `Set` | Assign values to workflow variables | Data |
| `Transform` | Transform data using expressions or code | Data |
| `Switch` | Conditional branching based on expressions | Control |
| `Loop` | Iterate over collections or until a condition | Control |
| `Merge` | Join multiple branches back together | Control |
| `Wait` | Pause execution for a duration or until event | Control |
| `Code` | Execute arbitrary code (JavaScript/Python/Rust) | Compute |
| `Database` | Query databases (SQL, key-value, document) | I/O |
| `Email` | Send emails via SMTP or provider APIs | I/O |
| `Notification` | Send notifications (Slack, webhook, push) | I/O |
| `RSS` | Read and parse RSS/Atom feeds | I/O |
| `File` | Read, write, and transform files | I/O |

## StandardProperties

Every action node carries `StandardProperties` — shared configuration that controls execution behavior:

```rust
use adk_action::{StandardProperties, ErrorHandling, RetryConfig};

let props = StandardProperties::builder()
    // Error handling
    .on_error(ErrorHandling::ContinueOnFail)
    .retry(RetryConfig {
        max_attempts: 3,
        wait_between_ms: 1000,
    })

    // Tracing
    .notes("Fetch user profile from API")

    // Callbacks
    .on_success("notify_complete")
    .on_failure("alert_team")

    // Execution
    .timeout_ms(30_000)
    .continue_on_fail(true)

    // Input/output mapping
    .input_mapping("{{trigger.body.user_id}}")
    .output_key("user_profile")

    .build();
```

### StandardProperties Fields

| Field | Type | Description |
|-------|------|-------------|
| `on_error` | `ErrorHandling` | `Stop`, `ContinueOnFail`, or `RetryThenFail` |
| `retry` | `Option<RetryConfig>` | Retry attempts and delay between them |
| `notes` | `Option<String>` | Human-readable description for tracing |
| `on_success` | `Option<String>` | Callback node to execute on success |
| `on_failure` | `Option<String>` | Callback node to execute on failure |
| `timeout_ms` | `Option<u64>` | Maximum execution time |
| `continue_on_fail` | `bool` | Whether downstream nodes execute after failure |
| `input_mapping` | `Option<String>` | Expression to transform input data |
| `output_key` | `Option<String>` | Variable name to store the output |

## Variable Interpolation

Action nodes support `{{variable}}` syntax for referencing workflow state:

```rust
use adk_action::HttpNode;

let node = HttpNode::builder()
    .url("https://api.example.com/users/{{trigger.body.user_id}}")
    .method("GET")
    .headers(vec![
        ("Authorization".into(), "Bearer {{env.API_TOKEN}}".into()),
    ])
    .build();
```

### Variable Sources

| Prefix | Source | Example |
|--------|--------|---------|
| `trigger` | Trigger node payload | `{{trigger.body.email}}` |
| `env` | Environment variables | `{{env.DATABASE_URL}}` |
| `nodes` | Output from previous nodes | `{{nodes.fetch_user.json.name}}` |
| `workflow` | Workflow-level variables | `{{workflow.run_id}}` |

Nested access uses dot notation: `{{nodes.http_1.json.data[0].id}}`

## Node Examples

### HTTP Node

```rust
use adk_action::{HttpNode, HttpMethod};

let node = HttpNode::builder()
    .method(HttpMethod::Post)
    .url("https://api.example.com/orders")
    .headers(vec![
        ("Content-Type".into(), "application/json".into()),
    ])
    .body(r#"{"item": "{{trigger.body.item}}", "qty": {{trigger.body.quantity}}}"#)
    .properties(StandardProperties::builder()
        .timeout_ms(10_000)
        .on_error(ErrorHandling::RetryThenFail)
        .retry(RetryConfig { max_attempts: 3, wait_between_ms: 2000 })
        .output_key("order_response")
        .build())
    .build();
```

### Switch Node

```rust
use adk_action::{SwitchNode, SwitchCase};

let node = SwitchNode::builder()
    .cases(vec![
        SwitchCase {
            condition: "{{nodes.classify.json.category}} == 'urgent'".into(),
            output: "urgent_path".into(),
        },
        SwitchCase {
            condition: "{{nodes.classify.json.category}} == 'normal'".into(),
            output: "normal_path".into(),
        },
    ])
    .fallback("default_path")
    .build();
```

### Loop Node

```rust
use adk_action::{LoopNode, LoopMode};

let node = LoopNode::builder()
    .mode(LoopMode::ForEach {
        items: "{{nodes.fetch_users.json.users}}".into(),
        item_var: "current_user".into(),
    })
    .body_nodes(vec!["process_user", "save_result"])
    .properties(StandardProperties::builder()
        .notes("Process each user in the list")
        .build())
    .build();
```

### Set Node

```rust
use adk_action::SetNode;

let node = SetNode::builder()
    .assignments(vec![
        ("status".into(), "processing".into()),
        ("started_at".into(), "{{workflow.timestamp}}".into()),
        ("user_email".into(), "{{trigger.body.email}}".into()),
    ])
    .build();
```

## Integration with adk-graph

Action nodes are executed by `adk-graph`'s `ActionNodeExecutor`:

```rust
use adk_graph::{Graph, ActionNodeExecutor};
use adk_action::{TriggerNode, HttpNode, SetNode};

// Define nodes
let trigger = TriggerNode::webhook("order_received");
let fetch = HttpNode::get("https://api.example.com/inventory/{{trigger.body.sku}}");
let update = SetNode::new(vec![("available", "{{nodes.fetch.json.quantity}}")]);

// Build graph
let graph = Graph::builder()
    .node("trigger", trigger)
    .node("check_inventory", fetch)
    .node("update_status", update)
    .edge("trigger", "check_inventory")
    .edge("check_inventory", "update_status")
    .build()?;

// Execute
let executor = ActionNodeExecutor::new();
let result = executor.run(graph, initial_context).await?;
```

## Defining Custom Node Types

Implement the `ActionNode` trait:

```rust
use adk_action::{ActionNode, ActionContext, ActionResult, StandardProperties};
use async_trait::async_trait;

struct CustomNode {
    config: MyConfig,
    properties: StandardProperties,
}

#[async_trait]
impl ActionNode for CustomNode {
    fn node_type(&self) -> &str { "custom" }
    fn properties(&self) -> &StandardProperties { &self.properties }

    async fn execute(&self, ctx: &ActionContext) -> ActionResult {
        let input = ctx.resolve("{{trigger.body.data}}")?;
        // Custom logic...
        Ok(serde_json::json!({ "result": "processed" }))
    }
}
```

## Related

- [Graph Agents](../agents/graph-agents.md) — Graph workflow orchestration
- [Studio Action Nodes](../studio/action-nodes.md) — Visual node editor in ADK Studio
- [Triggers](../studio/triggers.md) — Workflow trigger types

---

**Previous**: [← Retry & Reflect](retry-reflect.md) | **Next**: [Plugins →](../core/plugins.md)
