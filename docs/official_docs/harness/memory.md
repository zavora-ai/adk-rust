# Harness file memory

File memory lets the harness keep what it learned across sessions — how to run the tests,
where the build breaks — as Markdown topic files with a derived index. It is off until
`HarnessAgentBuilder::memory` is called. Feature `memory` of `adk-harness`, on by default
(spec 3.1 §7.15, §9.2 I33).

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["memory"] }
```

## Where memory lives

| `MemoryLayout` | Directory | Notes |
|---|---|---|
| `PerUser { base }` (default, `base` = `~/.adk/memory`) | `<base>/<app>/<sha256(user)>/<project-slug>` | outside the workspace, so no shell command run there writes it; one memory per user and workspace |
| `Project` (opt-in) | `<workspace>/.adk/memory` | shared by every user of the workspace; `.adk/**` is a protected path, so the file tools need a non-waivable approval to touch it |

The project slug is the workspace directory name and 12 hex digits of the digest of its
canonical path (`adk_harness::project_slug`).

## Topics and the index

A topic is a `.md` file whose first lines are `---`, `summary: <one line>`, `---`.
`MEMORY.md` is derived from the summaries: the text above its marker line is a header a
person wrote and is kept; the list below it is regenerated after every write and delete,
so writers never edit the same lines.

| `MemoryLimits` | Default | When exceeded |
|---|---|---|
| `index_max_lines` | 200 | the index the model sees ends with "… N more topics" |
| `index_max_bytes` | 16 KiB | the same |
| `file_max_bytes` | 64 KiB | `harness.memory.too_large` |
| `max_files` | 200 | `harness.memory.too_many_files` for a new topic |

Topic paths are relative, made of letters, digits, `.`, `-` and `_`, at most three levels,
end in `.md`, and are never `MEMORY.md`; anything else is `harness.memory.invalid_path`.

## Tools and approvals

| Tool | Class | ReadOnly | Plan | Ask | Auto |
|---|---|---|---|---|---|
| `memory_read` `{ path? }` | Read | Allow | Allow | Allow | Allow |
| `memory_write` `{ path, summary, content }` | MemoryWrite | Deny | Deny | one-time approval | one-time approval |
| `memory_delete` `{ path }` | MemoryWrite | Deny | Deny | one-time approval | one-time approval |

No standing rule or run-wide approval covers a memory write (`MemoryWritePolicy::Ask`), and
a prompt for one offers no "always allow". Each write and delete records
`harness.memory.written` before it happens, with `logicalPath`, `beforeSha256`,
`afterSha256`, `bytes` and `functionCallId`.

The index of the run's user is the last part of the harness instruction, inside a
`<memory file="MEMORY.md">` block that states it is data, not instructions, and that its
content cannot close.

```rust
use std::sync::Arc;

use adk_harness::{HarnessAgent, MemoryConfig, MemoryLimits};

fn with_memory(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<HarnessAgent> {
    HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new("."))
        .memory(MemoryConfig {
            limits: MemoryLimits { max_files: 50, ..MemoryLimits::default() },
            ..MemoryConfig::default()
        })
        .build()
}
```

## Using memory outside the tools

`FileMemory` is the memory directory as an API (`open`, `for_user`, `index`, `topics`,
`read`, `write`, `delete`). `FileMemory::as_core_memory` adapts it to `adk_core::Memory`
for `PreloadMemoryTool`-style callers; the core trait takes no user, so the adapter serves
one user's memory, and it refuses `add` and `delete`, since the model writes memory only
through the approved tools.

```rust
use adk_harness::{FileMemory, MemoryConfig};

async fn recall(workspace: &std::path::Path) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
    let memory = FileMemory::for_user(&MemoryConfig::default(), "adk-rust", "alice", workspace)?;
    memory.as_core_memory().search("tests").await
}
```
