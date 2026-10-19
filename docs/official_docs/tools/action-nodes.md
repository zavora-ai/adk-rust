# Action Nodes

The `adk-action` crate defines 14 action node types used in graph-based workflows. Each node type represents a discrete operation (HTTP call, data transform, conditional branch, etc.) that can be composed into directed graphs via `adk-graph`'s `ActionNodeExecutor`.

## Overview

Action nodes are the building blocks of visual and programmatic workflow graphs:

- **`adk-action`** defines the node configurations: plain serde types (camelCase JSON) for
  14 node types, the shared `StandardProperties`, and `{{variable}}` interpolation. It has no
  executor.
- **`adk-graph`** (`action` feature) runs them: `ActionNodeExecutor` wraps one
  `ActionNodeConfig` as a graph `Node`, and `WorkflowSchema` builds a whole graph from JSON —
  the interchange format ADK Studio writes.

## Installation

```toml
[dependencies]
adk-action = "3.0.0"
adk-graph = { version = "3.0.0", features = ["action"] }

# Or through the umbrella crate (re-exports `adk_action` and `adk_rust::graph`)
adk-rust = { version = "3.0.0", features = ["action"] }
```

| Feature (`adk-graph` / `adk-rust`) | Adds |
|------------------------------------|------|
| `action` | `ActionNodeExecutor`, `WorkflowSchema`, every node without an external backend |
| `action-http` | HTTP and Notification nodes |
| `action-rss` | RSS node (implies `action-http`) |
| `action-trigger` | `TriggerRuntime` for webhook, schedule, and event triggers |
| `action-db`, `action-email`, `action-code` | Placeholders only; see below |
| `action-full` | All of the above |

## Node Types (14)

`ActionNodeConfig` is tagged by `type`:

| `type` | What the executor does |
|--------|------------------------|
| `trigger` | Writes trigger metadata — type, timestamp, and `input`: the state value under `inputLabel` (default `input`), or `defaultPrompt`. `TriggerRuntime` (`action-trigger`) fires webhook, schedule, and event triggers |
| `http` | Sends the request (`action-http`) under an `HttpActionPolicy` |
| `set` | `set` writes each variable, `merge` deep-merges it into the existing value, `delete` writes `null`; `envVars` loads environment variables into state |
| `transform` | `template` interpolates a string; `jsonpath` reads a dot path; `builtin` is not implemented and writes `null` with a warning. Optional coercion to string, number, boolean, or array |
| `switch` | Evaluates `{field, operator, value}` conditions (`eq`, `neq`, `gt`, `lt`, `gte`, `lte`, `contains`, `startsWith`, `endsWith`, `matches`, `in`, `empty`, `exists`) and writes the matching output port, or every matching port in `all_match` mode; without a match and a `defaultBranch` the node fails |
| `loop` | Records iterations and does not run other nodes: `forEach` writes the item and index variables (the last item wins) and `{"iterations": n}`; `while` evaluates its condition once (0 or 1 iterations); `times` counts |
| `merge` | Collects the state keys prefixed `branch:` (`waitAll`, `waitAny`, `waitN`) and combines them as an array, object, first, or last value |
| `wait` | Waits a fixed duration (`ms`, `s`, `m`, `h`), until an RFC 3339 time, or until a condition holds; the `webhook` wait type is not implemented and writes `null` with a warning |
| `code` | `rust`: stores `code` parsed as JSON, or the interpolated text; it does not compile or run Rust. `javascript` and `typescript` are rejected |
| `database` | Not implemented; rejected |
| `email` | Not implemented; rejected |
| `notification` | Posts a Slack, Discord, Teams, or generic webhook payload (`action-http`) |
| `rss` | Reads and parses a feed (`action-rss`) |
| `file` | Reads, writes, deletes, or lists local files inside the allowed roots; a read parses `json`, `csv`, or text |

## Availability Is Checked When the Graph Is Built

Some node types accept and validate a configuration while their backend does not exist.
Those are refused by `StateGraph::compile()`, not part-way through a run:

| Configuration | Status |
|---------------|--------|
| `Database` (any type) | Rejected — no driver is integrated |
| `Email` (monitor or send) | Rejected — IMAP and SMTP are not implemented |
| `Code` with `language: javascript` or `typescript` | Rejected — no sandboxed runtime; use `rust` |
| `Http` without the `action-http` feature | Rejected — enable the feature |

A rejection names the node and the reason, so a workflow that cannot run fails while it
is being assembled rather than after earlier nodes have already had side effects.
Notification and RSS nodes built without their features, a `builtin` transform, and a
`webhook` wait are not rejected here: the first two fail when they run, and the last two
write `null`.

Implement `Node::validate` on a custom node to take part in the same check.

A node that bypasses the build-time check and executes anyway fails with an error
saying its node type is not implemented. Enabling `action-db`, `action-email`, or
`action-code` does not change that: those features compile the placeholders, not a
backend.

## Untrusted Paths and URLs

File paths and HTTP URLs are interpolated from workflow state, so `ActionNodeExecutor`
confines both.

**File nodes** operate only inside the allowed roots — the current working directory
unless `with_file_roots` names others:

| Path | Outcome |
|------|---------|
| Relative | Resolved against the first root |
| Contains a `..` component | Rejected |
| Resolves outside every root, including through a symbolic link | Rejected |
| Ends in a dangling symbolic link | Rejected |

Listing does not follow or report symbolic links. Reported paths are the resolved
absolute paths.

**HTTP nodes** check the URL, and every redirect, against an `HttpActionPolicy`:

| Setting | Default | Builder |
|---------|---------|---------|
| Schemes | `https` and `http`; `file:` and others are rejected | `allow_schemes` |
| Hosts | Any | `allow_hosts` — exact names or `*.example.com` |
| Redirects | Up to 5, each re-checked | `max_redirects`; `0` follows none |

Logs and errors show the URL without its query string or credentials. The policy applies
to HTTP nodes; Notification and RSS nodes send their requests without it.

```rust,ignore
use adk_graph::action::ActionNodeExecutor;
use adk_graph::action::http::HttpActionPolicy;

let files = ActionNodeExecutor::new(file_config).with_file_roots(["/srv/workflow-data"]);
let fetch = ActionNodeExecutor::new(http_config).with_http_policy(
    HttpActionPolicy::new().allow_schemes(["https"]).allow_hosts(["api.example.com"]),
);
```

## StandardProperties

Every node configuration flattens `StandardProperties` into its JSON object:

| Field | JSON | Description |
|-------|------|-------------|
| `id`, `name` | `"id"`, `"name"` | The node ID is its name in the graph |
| `description`, `position` | optional | Presentation metadata |
| `error_handling` | `"errorHandling": {"mode", "retryCount", "retryDelay", "fallbackValue"}` | See below |
| `tracing` | `"tracing": {"enabled", "logLevel"}` | `logLevel`: `none`, `error`, `info`, `debug` |
| `callbacks` | `"callbacks": {"onStart", "onComplete", "onError"}` | Optional names |
| `execution` | `"execution": {"timeout", "condition"}` | `timeout` in milliseconds (default 30000) bounds the whole node; the node is skipped when `condition` interpolates to empty or `false` |
| `mapping` | `"mapping": {"inputMapping", "outputKey"}` | `outputKey` is the state key the node writes its result to |

| `errorHandling.mode` | On failure |
|----------------------|-----------|
| `stop` | The node, and the graph run, fail |
| `continue` | The error is logged and the node produces no output |
| `retry` | The node runs `retryCount + 1` times (`retryCount` defaults to 1), `retryDelay` milliseconds apart |
| `fallback` | `fallbackValue` (or `null`) is written to `outputKey` |

## Variable Interpolation

String fields support `{{key}}` and dot paths such as `{{user.address.city}}`, resolved
against the graph state when the node runs:

| Case | Result |
|------|--------|
| The key or path exists | Its value; a non-string value is written as JSON text |
| It is missing or `null` | An empty string |
| A path with an index, such as `{{items[0]}}` | Not a variable; left as written |

There are no reserved prefixes: `{{env.API_TOKEN}}` reads the state key `env`, not the
process environment. Load environment variables into state with a Set node's `envVars`.
`adk_action::interpolate_variables` and `get_nested_value` apply the same rules.

## Run Action Nodes in a Graph

Build each node from its JSON configuration, wrap it in an `ActionNodeExecutor`, and add it to
a `StateGraph`:

```rust
use adk_action::ActionNodeConfig;
use adk_graph::action::ActionNodeExecutor;
use adk_graph::{END, ExecutionConfig, START, State, StateGraph};
use serde_json::json;

let standard = |id: &str, output_key: &str| {
    json!({
        "id": id,
        "name": id,
        "errorHandling": { "mode": "stop" },
        "tracing": { "enabled": false, "logLevel": "none" },
        "callbacks": {},
        "execution": { "timeout": 30000 },
        "mapping": { "outputKey": output_key }
    })
};

let mut set = standard("load_user", "load_user_result");
set["type"] = json!("set");
set["mode"] = json!("set");
set["variables"] = json!([{ "key": "user", "value": { "name": "Ada", "plan": "pro" } }]);

let mut greet = standard("greet", "greeting");
greet["type"] = json!("transform");
greet["transformType"] = json!("template");
greet["template"] = json!("Hello {{user.name}}, your plan is {{user.plan}}.");

let set: ActionNodeConfig = serde_json::from_value(set)?;
let greet: ActionNodeConfig = serde_json::from_value(greet)?;

let graph = StateGraph::with_channels(&["user", "greeting"])
    .add_node(ActionNodeExecutor::new(set))
    .add_node(ActionNodeExecutor::new(greet))
    .add_edge(START, "load_user")
    .add_edge("load_user", "greet")
    .add_edge("greet", END)
    .compile()?;

let state = graph.invoke(State::new(), ExecutionConfig::new("thread-1")).await?;
assert_eq!(state["greeting"], json!("Hello Ada, your plan is pro."));
```

`WorkflowSchema::from_json(json)?.build_graph(name)?` builds a `GraphAgent` from a
workflow document — `actionNodes` keyed by node ID, `edges`, and `conditions` — and adds the
routing edges for each Switch node. It wraps every node with `ActionNodeExecutor::new`, so
file roots and the HTTP policy keep their defaults; build the graph yourself to set them.
`trigger_configs()` and `build_trigger_runtime(...)` (`action-trigger`) expose the workflow's
triggers.

## Defining Custom Node Types

The node types are a closed enum. For custom logic, implement `adk_graph::Node` (or use
`StateGraph::add_node_fn`) and add the node to the same graph beside `ActionNodeExecutor`
nodes; implement `Node::validate` to take part in the build-time check.

## Related

- [Graph Agents](../agents/graph-agents.md) — Graph workflow orchestration
- [Studio Action Nodes](../studio/action-nodes.md) — Visual node editor in ADK Studio
- [Triggers](../studio/triggers.md) — Workflow trigger types

---

**Previous**: [← Retry & Reflect](retry-reflect.md) | **Next**: [Plugins →](../core/plugins.md)
