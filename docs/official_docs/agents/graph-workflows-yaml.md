# YAML Workflows

A graph workflow can be a file. `adk_graph::definition` reads a YAML or JSON workflow, checks it,
and compiles it into the same `CompiledGraph` a `StateGraph` builds. The file declares the nodes
(agents, LLM agents, function, `set`, action and interrupt nodes), the edges and their
conditional routes, the state channels, the interrupts and the checkpoint store. Rust code
supplies only what the file names: agents, models, tools and functions, through a
`WorkflowRegistry`.

| You write | You get |
|-----------|---------|
| A `.yaml` or `.json` workflow file | A `CompiledGraph`, or a `GraphAgent` for a `Runner` |
| A wrong node id, field or operator | Every issue at once, each with its path, line and column |
| `# yaml-language-server: $schema=...` | Completion and validation in the editor, from the committed JSON Schema |
| `adk-rust graph run refund_desk.yaml` | The run, with pauses answered on stdin |

## Installation

```toml
[dependencies]
adk-graph = { version = "3.0.0", features = ["yaml"] }
```

Through the umbrella crate, the `graph-yaml` feature (in `full`) turns it on, and the module is
`adk_rust::graph::definition`. `yaml` implies `action`, so `action:` nodes run every
[action node](../tools/action-nodes.md) type `action` compiles.

## A Complete Workflow

This workflow classifies a customer message with one LLM agent, holds a refund for a person's
approval, and replies with a second LLM agent. It is `adk-graph/tests/workflows/refund_desk.yaml`.

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/zavora-ai/adk-rust/main/adk-graph/schemas/workflow.schema.json
version: 1
name: refund_desk
description: Classify a customer message, hold refunds for a person's approval, and reply.

models:
  fast: anthropic/claude-haiku-5-5

nodes:
  classify:
    llm:
      model: fast
      instruction: >-
        Classify the customer's message. Reply with exactly one lowercase word:
        refund if they ask for money back, otherwise question.
    input: "{{input}}"
    output_key: category

  approve:
    interrupt:
      message: "Refund requested: {{input}} -- approve it?"
      response_schema:
        type: object
        properties:
          approved: { type: boolean }
        required: [approved]
    output_key: approval

  refund_reply:
    llm:
      model: fast
      instruction: >-
        Write one short sentence telling the customer their refund is approved
        and will arrive within five working days.
    input: "{{input}}"
    output_key: reply

  decline_reply:
    llm:
      model: fast
      instruction: Write one short sentence telling the customer their refund was declined.
    input: "{{input}}"
    output_key: reply

  answer:
    llm:
      model: fast
      instruction: Answer the customer's question in one short sentence.
    input: "{{input}}"
    output_key: reply

  close:
    set:
      status: "closed: {{category}}"

edges:
  - [START, classify]
  - from: classify
    routes:
      - when: { field: category, op: matches, value: "(?i)refund" }
        to: approve
    default: answer
  - from: approve
    routes:
      - when: { field: approval.approved, op: eq, value: true }
        to: refund_reply
    default: decline_reply
  - from: refund_reply
    to: close
  - from: decline_reply
    to: close
  - from: answer
    to: close
  - [close, END]
```

`close` has three incoming edges, so it waits for the predecessors that can still run in the
pass. After a conditional branch, that is the one branch taken.

## Loading and Running in Rust

```rust
use std::sync::Arc;

use adk_core::Llm;
use adk_graph::definition::{WorkflowDefinition, WorkflowRegistry};
use adk_graph::{ExecutionConfig, GraphError, State};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = std::env::var("ANTHROPIC_API_KEY")?;
    let haiku: Arc<dyn Llm> =
        Arc::new(AnthropicClient::new(AnthropicConfig::new(key, "claude-haiku-5-5"))?);
    let registry = WorkflowRegistry::new().model("anthropic/claude-haiku-5-5", haiku);

    let graph = WorkflowDefinition::from_file("refund_desk.yaml")?.build(&registry).await?;

    let input = State::from([("input".to_string(), json!("I want my money back for order 42"))]);
    match graph.invoke(input, ExecutionConfig::new("ticket-42")).await {
        Err(GraphError::Interrupted(paused)) => println!("paused: {:?}", paused.interrupt),
        other => println!("{other:?}"),
    }

    // A person approves: the interrupt node writes the answer to `approval` and the run
    // continues on the same thread.
    let state = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("ticket-42")
                .with_resume_value("approve", json!({ "approved": true })),
        )
        .await?;
    println!("{}", state["reply"]);
    Ok(())
}
```

`build_agent` returns a `GraphAgent` named after the workflow, which runs under a `Runner`; each
turn of a paused thread resumes it with the turn's text, as for any graph agent.

### The registry

| Method | Supplies |
|--------|----------|
| `.agent(Arc<dyn Agent>)` | An agent for `agent: <name>`, keyed by its name |
| `.model(name, Arc<dyn Llm>)` | A model for `llm.model` and `models` aliases, keyed by its reference |
| `.model_resolver(fn(&str) -> Result<Arc<dyn Llm>>)` | Models no registered name answers, such as `openai/gpt-5.5` |
| `.tool(Arc<dyn Tool>)` | A tool for `llm.tools`, keyed by its name |
| `.function(name, async fn(NodeContext) -> Result<NodeOutput>)` | A function for `function: <name>` |
| `.checkpointer(Arc<dyn Checkpointer>)` | The checkpoint store, in place of the file's |

## The Format

### Top-level keys

| Key | Required | Meaning |
|-----|----------|---------|
| `version` | yes | Must be `1` |
| `name` | yes | The workflow's name; a `GraphAgent` takes it |
| `description` | no | What the workflow does |
| `nodes` | yes | The nodes, by id: letters, digits, `_` and `-`, starting with a letter or `_` |
| `edges` | yes | The edges, in order |
| `state` | no | Channels with a reducer (`overwrite`, `append`, `sum`) or a `default` |
| `models` | no | Aliases for model references |
| `interrupt_before`, `interrupt_after` | no | Node ids the run pauses before or after |
| `checkpoint` | no | `store: memory` (default), `sqlite` with a `path` relative to the file, or `none` |
| `recursion_limit` | no | The most supersteps one run may take; the graph's default is 100 |

A key no channel names is overwritten by each update. `messages`, which agent nodes append their
text to, appends unless `state` declares it.

### Nodes

Each node sets exactly one kind.

| Kind | Example | What it does |
|------|---------|--------------|
| `agent` | `agent: researcher` | Runs a registered agent |
| `llm` | `llm: { model: fast, instruction: ..., tools: [search] }` | Runs an `LlmAgent` declared here, on a model alias, a registered model, or a `provider/model` reference |
| `function` | `function: score` | Runs a registered function node |
| `set` | `set: { status: "closed: {{category}}" }` | Writes state keys; a string is a template over state |
| `action` | `action: { type: transform, transformType: template, template: "..." }` | Runs an `adk-action` node; its `id`, `name`, error handling (`stop`), tracing (off), 30 s timeout and output key default from the node |
| `interrupt` | `interrupt: { message: "Approve?", response_schema: {...} }` | Pauses for an answer; a resume writes it to `output_key` (default: the node id) and the run continues |

| Node key | Applies to | Meaning |
|----------|------------|---------|
| `input` | `agent`, `llm` | A template for the message the agent receives; defaults to the last message in `messages`, else `input` |
| `output_key` | `agent`, `llm`, `interrupt`, `action` | Where the node writes: the agent's final text, the answer, or the action's result |
| `max_attempts` | every kind | Attempts for a failing node, including the first |
| `description` | every kind | What the node does |

Templates are `adk-action` interpolation: `{{key}}` and `{{key.field}}` read state, and a name
that is not set becomes empty text. An `llm` instruction keeps `LlmAgent`'s own `{key}` session
placeholders.

### Edges

An edge item is a chain or an edge object. `START` and `END` are the graph's entry and exit.

| Form | Example | Meaning |
|------|---------|---------|
| Chain | `- [START, classify, END]` | An edge between each consecutive pair |
| Direct | `- { from: a, to: b }` | Always continue to `b` |
| Fan-out | `- { from: a, to: [b, c] }` | Run `b` and `c` in parallel |
| Routes | `- { from: a, routes: [{ when: ..., to: b }], default: c }` | The first route whose `when` holds; else `default`, else `END` |
| Switch | `- { from: a, switch: category, cases: { refund: b }, default: c }` | The node for the state value at `switch`, compared as text after trimming |

An edge from `START` cannot be conditional; route from the first node instead.

### Conditions

A condition is data, never code: a comparison, or `all`, `any` or `not` of conditions. The
operators are `adk-action`'s switch operators, so a route and a switch node agree.

```yaml
when:
  all:
    - { field: approval.approved, op: eq, value: true }
    - { field: amount, op: lte, value: 500 }
    - not: { field: customer.tier, op: in, value: [blocked, suspended] }
```

| `op` | Holds when the value at `field` |
|------|---------------------------------|
| `eq`, `neq` | equals, or does not equal, `value`; numbers compare numerically, booleans as `true`/`false` |
| `gt`, `lt`, `gte`, `lte` | compares numerically with `value` |
| `contains` | is a string containing `value`, or a list holding it |
| `starts_with`, `ends_with` | is a string starting or ending with `value` |
| `matches` | matches the regular expression `value`; `(?i)` makes it case-insensitive |
| `in` | is one of `value`, a list |
| `empty` | is absent, null or empty; takes no `value` |
| `exists` | is present; takes no `value` |

`field` is a dotted state path. An unknown operator is a parse error, and a `matches` pattern is
compiled when the file loads, so a typo fails before the run.

## Validation and Errors

`from_yaml_str`, `from_json_str` and `from_file` parse and validate, and `validate` checks a
definition again. Validation reports every issue at once:

- a node with no kind or two kinds
- an edge to an unknown node, or `START` or `END` out of place
- an edge with no target, or two
- a condition with a missing or extra `value`, or an invalid regular expression
- a node no edge reaches from `START`
- a workflow that pauses with `checkpoint.store: none`

`build` adds the names the registry cannot resolve: agents, models, tools and functions.

```text
workflow.yaml:11:16: edges[0][2]: unknown node 'missing'; declared nodes are a, ask, b, orphan
workflow.yaml:14:47: edges[1].routes[0].when.value: not a valid regular expression: ...
workflow.yaml:4:15: nodes.a.agent: no agent named 'ghost' is registered; registered agents: none
```

| Source | Each issue carries |
|--------|--------------------|
| YAML | its path, line and column, from the parser for a syntax or type error and from a location index for a validation issue |
| JSON | its path; a syntax or type error carries the parser's line and column |

`WorkflowError` holds every `WorkflowIssue` (`path`, `location`, `message`) and converts to
`GraphError::InvalidGraph` and `AdkError`.

## Editor Support

`adk_graph::definition::workflow_json_schema()` returns the format's JSON Schema, and the
committed copy is `adk-graph/schemas/workflow.schema.json`. A test fails when the copy drifts
from the types. A YAML file opts in with the first-line comment shown in the example, and
`adk-rust graph schema` prints the schema.

## From the Command Line

```bash
adk-rust graph validate refund_desk.yaml
adk-rust graph run refund_desk.yaml --input "I want my money back for order 42"
adk-rust graph schema > workflow.schema.json
```

| Option | Meaning |
|--------|---------|
| `--input <text>` | Written to the `input` state key |
| `--state <json>` | The initial state, a JSON object |
| `--thread <id>` | The thread; defaults to `workflow` |
| `--db <path>` | A SQLite checkpoint database, in place of the file's store |
| `--resume <json>` | Resumes the paused thread with this answer; a bare word is a string |
| `--node <id>` | The paused node the answer is for; defaults to the thread's only pending node |

`graph run` builds each `provider/model` reference the file names with the key the other
commands use: the environment, the keychain or the saved config. With an in-memory store, a
pause prints its question and reads the answer from stdin, so
`printf '{"approved": true}\n' | adk-rust graph run ...` runs to the end. With `--db` or a
`sqlite` store, the pause outlives the process and the command prints the `--resume` invocation
that continues it. `adk-rust graph steps <thread> --db <path>` then shows the thread's steps.

## Compared With ADK Python 2.9

ADK Python 2.9 loads graph workflows from YAML agent configs. This format keeps its shape where
it carries over to Rust:

| ADK Python 2.9 | ADK-Rust |
|----------------|----------|
| Block-list chains, `START` | Chains, `START` and `END` |
| A routing map `{route: node}` on an emitted route | `switch` with `cases`, or `routes` with conditions; `default` for the fallback |
| `agent_class: LlmAgent` with `model` and `instruction` | `llm:` nodes; `agent:` for a registered agent |
| `*_code` references to Python callables | `function:` names a function registered in Rust; the file carries no code |
| `RequestInput` in code | `interrupt:` nodes and `interrupt_before`/`interrupt_after` in the file |
| Runtime-managed resume | `checkpoint` in the file, resume by thread |

---

**Previous**: [← Graph Checkpointing](./graph-checkpointing.md) | **Next**: [Graph Agents →](./graph-agents.md)
