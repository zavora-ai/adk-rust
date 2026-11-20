# `CodingAgent` (2.x)

> **Note:** this page describes `adk_agent::coding::CodingAgent`, the 2.x API. The
> [harness](../harness/index.md) (spec 3.1, in 3.0.0) supersedes it; new projects start from
> the [harness quick start](../harness/index.md#quick-start). `adk_agent::coding` is unchanged
> in 3.0.0 and carries no `#[deprecated]` attribute;
> [Migrating from `CodingAgent`](../migration/coding-agent-to-harness.md) moves a project to
> the harness.

`CodingAgent` is a thin harness — shipped in `adk-agent` behind the `coding`
feature — that wires the [dev tools](devtools.md), a planning `write_todos` tool,
and a minimal coding system prompt onto a normal `LlmAgent`. It's *configuration
over `LlmAgent`*, not a new agent type, so it keeps the default `adk-agent` build
lean (the feature pulls in `adk-devtools`).

## Build one

```rust
use adk_agent::coding::CodingAgent;
use adk_devtools::Workspace;

let coding = CodingAgent::builder()
    .model(model)                              // any adk-model provider (required)
    .workspace(Workspace::new("./my-repo").allow_bash(true)) // sandboxed (required); bash is opt-in
    .instruction("Follow the project's existing style; prefer small diffs.") // optional, appended
    .build()?;

let agent = coding.agent();   // Arc<dyn Agent> — hand to a Runner
```

### Run one

```toml
[dependencies]
adk-agent = { version = "3.0.0", features = ["coding"] }
adk-core = "3.0.0"
adk-devtools = "3.0.0"
adk-runner = "3.0.0"
adk-session = "3.0.0"
anyhow = "1"
```

```rust
use adk_agent::coding::CodingAgent;
use adk_core::{Content, SessionId, UserId};
use adk_devtools::Workspace;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use std::sync::Arc;

async fn run(model: Arc<dyn adk_core::Llm>) -> anyhow::Result<()> {
    // One call builds a coding agent over a confined workspace; bash is opt-in.
    let coding = CodingAgent::builder()
        .model(model)
        .workspace(Workspace::new("./my-repo").allow_bash(true))
        .build()?;

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "demo".into(),
            user_id: "u".into(),
            session_id: Some("s".into()),
            state: Default::default(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("demo")
        .agent(coding.agent())
        .session_service(sessions)
        .build()?;

    let _events = runner
        .run(
            UserId::new("u")?,
            SessionId::new("s")?,
            Content::new("user").with_text("Add a function add(a,b) to add.py and run it."),
        )
        .await?;
    // Stream the events: FunctionCall, FunctionResponse, Text.
    Ok(())
}
```

Builder options:

| Method | Effect |
|--------|--------|
| `.model(Arc<dyn Llm>)` | The model (required). |
| `.workspace(Workspace)` | The workspace the tools are confined to (required). |
| `.name("…")` | Agent name (default `"coding-agent"`). |
| `.instruction("…")` | Extra guidance appended to the base coding prompt. |
| `.tool(Arc<dyn Tool>)` | Register an extra tool (MCP, function tool, …). |
| `.without_todos()` | Disable the `write_todos` planning tool. |

## What's wired

- **The dev toolset** — read/write/edit/glob/grep/bash, scoped to the workspace.
- **`write_todos`** — a planning tool the model uses to record and update a short
  task list. Each call replaces the list of the session it runs in, stored in
  session state under `harness:todos` (`adk_agent::coding::TODOS_STATE_KEY`) as
  `{ "items": [{ "id", "content", "status" }], "revision" }`, so sessions sharing one
  agent keep separate plans. UIs see each write in
  `event.actions.state_delta["harness:todos"]`. `coding.todos()` returns the last
  list any session wrote in this process, which is the running session's list when
  one session runs at a time:

  ```rust
  for todo in coding.todos() {
      println!("[{}] {}", todo.status, todo.content);  // pending | in_progress | completed
  }
  ```

  For a specific session, read the key from its state:

  ```rust
  use adk_agent::coding::TODOS_STATE_KEY;
  use adk_session::GetRequest;

  let session = sessions
      .get(GetRequest {
          app_name: "my-app".into(),
          user_id: "user".into(),
          session_id: "session-1".into(),
          num_recent_events: None,
          after: None,
      })
      .await?;
  let todos = session.state().get(TODOS_STATE_KEY); // Some({"items": [...], "revision": n})
  ```

- **A minimal prompt** — sub-1k-token base instructions ("explore before you
  change", "read before you edit", "verify by running tests", "track a plan"),
  kept small on purpose so capabilities come from tools and skills rather than a
  huge prompt.

## The loop

You run a `CodingAgent` like any agent — through a `Runner`. Within a turn the
underlying `LlmAgent` already executes a **plan → act → observe** loop: it calls
tools, sees their results, and continues until it's done. A typical turn:

```text
write_todos(...)            # plan
glob / grep / read_file     # explore
edit_file / write_file      # change
bash("…test…")              # verify
write_todos(... completed)  # update plan
→ final summary
```

For **multi-turn** work, reuse the same `Runner` + session across calls and the
agent builds on its own prior work (it re-reads and edits the files it wrote).
See the [`coding_agent` example](examples.md#coding_agent) (`multiturn` mode).

## Memory & skills

Because it's an `LlmAgent` underneath, everything else in ADK composes:

- Attach a [`MemoryService`](../memory/index.md) (e.g. the bi-temporal knowledge
  graph) on the `Runner` for cross-session project memory.
- Add skills with `adk-skill` to extend capabilities without bloating the prompt.
- Hooks/guardrails (`adk-plugin`, `adk-guardrail`) gate tool calls deterministically.

## Running it

The harness produces an `Arc<dyn Agent>`; drive it with `adk-runner` (see the
[quick start](index.md#60-second-quick-start)) or use the
[CLI](cli.md), which wraps all of this.

Next: [The CLI →](cli.md)
