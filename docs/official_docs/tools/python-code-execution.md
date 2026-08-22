# Python Code Execution (Monty)

ADK-Rust runs model-written Python **in-process** via the
[Pydantic Monty](https://github.com/pydantic/monty) interpreter — no container,
no subprocess, microsecond startup. The capability ships in two layers:

- **`adk-code`** (`embedded-python` feature) — `MontyExecutorBuilder` and the
  two executor products, `MontyOneShotExecutor` and `MontyReplExecutor`, both
  implementing `CodeExecutor`.
- **`adk-tool`** (`code-embedded-python` feature) — `MontyPythonCodeTool`
  (`monty_python_code`), the agent-facing tool over those executors.

This is a complement to the container-backed `PythonCodeTool` (`python_code`),
which runs full CPython in Docker — use that when scripts need the real Python
ecosystem (pip packages, C extensions, the complete standard library). Monty
implements a subset of Python, in exchange for in-process speed, serializable
interpreter state, and a no-network/no-subprocess guarantee that holds by
construction.

```toml
[dependencies]
adk-tool = { version = "2.1.0", features = ["code-embedded-python"] }
```

Or through the umbrella crate:

```toml
[dependencies]
adk-rust = { version = "2.1.0", features = ["minimal", "code-embedded-python"] }
```

## One-shot vs. REPL

One builder produces both products; the mode is encoded in the type, not a
flag:

| Mode | Build | State | Concurrency |
|------|-------|-------|-------------|
| One-shot | `build_one_shot()` | Fresh interpreter per call | Concurrency-safe |
| REPL | `build_repl()` | Variables, functions, and imports persist across calls | Calls serialize per session |

```rust
use adk_code::{MontyExecutorBuilder, PathAccess};

let builder = MontyExecutorBuilder::new()
    .allow_path("/data", "/srv/agent/data", PathAccess::ReadOnly)
    .allow_path("/out", "/srv/agent/out", PathAccess::ReadWrite)
    .environ_var("PROJECT", "acme")
    .system_clock();

let one_shot = builder.clone().build_one_shot()?;
let repl = builder.build_repl()?;
```

The REPL executor stores the serialized interpreter between calls. Monty
preserves the session through Python-level exceptions, so a failed snippet
does not destroy accumulated state. `CodeExecutor` lifecycle methods manage
the session: `start()` initializes it, `stop()` drops it, `restart()` resets
it, and `execute()` before `start()` lazily initializes.

## Security model

Isolation combines explicit policy with enforcement by omission:

- **Filesystem.** Only directories granted with `allow_path` are reachable,
  each read-only or read-write, through `pathlib.Path` against the *virtual*
  mount path. Monty's mount table enforces the boundary (canonicalization +
  symlink-escape detection). Any other path raises a catchable `OSError`
  (existence checks return `False`).
- **Environment.** `os.getenv` / `os.environ` read only the explicit map
  granted at construction — the host process environment is never exposed.
- **Clock.** `date.today()` / `datetime.now()` work only when
  `.system_clock()` was granted; otherwise they raise `OSError`.
- **Network and subprocess.** Monty has no surface for either — impossible
  regardless of configuration.
- **Timeouts.** `SandboxPolicy::timeout` maps to Monty's
  `ResourceLimits::max_duration` (real in-VM preemption, per call). A memory
  cap (default 256 MiB) bounds the heap; in REPL mode it bounds the
  *cumulative* session heap.

**Grants vs. request policy.** The builder's grants are the maximum access any
script can have. The per-request `SandboxPolicy` may only narrow within them —
a request exceeding the grants is rejected fail-closed with
`ExecutionError::UnsupportedPolicy` naming the excess, before any code runs.
A grant covers its entire directory subtree: requesting a granted mount *or
any subdirectory of one* succeeds, and the effective mount is the requested
path backed by the matching host subdirectory. Use `granted_policy()` to
request exactly what the executor offers.

A REPL session's effective policy must not vary between calls; a call whose
policy differs from the session's established policy is rejected with guidance
to `restart()`.

## Host functions

Registered Rust functions (sync or async) become callable Python functions,
visible to scripts by bare name:

```rust
use adk_code::MontyExecutorBuilder;
use serde_json::json;

let executor = MontyExecutorBuilder::new()
    .function_fn("row_count", "Count rows in the loaded dataset.", |args, _kwargs| async move {
        Ok(json!(args.len()))
    })
    .build_one_shot()?;
```

For the full trait form, implement `HostFunction` (`name`, `description`,
optional `signature` for the LLM prompt, and async `call` with JSON-converted
positional and keyword arguments). Registry validation happens at `build_*()`:
names must be valid Python identifiers, unique, and must not collide with
Python built-ins.

Inside a script, host functions are called synchronously — never with
`await`. A returned `Err` becomes a catchable Python exception carrying the
message; a call to an unregistered name raises a corrective exception listing
the registered names. Host-function execution has its own wall-clock bound
(`host_function_timeout`, default 30 s), so a hung function cannot wedge
`execute()`.

> **Note:** host functions run as host code. They are the user's own trust
> boundary, not Monty's — the interpreter sandbox does not contain their side
> effects.

## Self-describing executors

Both executors implement `CodeExecutor::prompt_snippet()`, rendering their
**built** capabilities: mode semantics, filesystem roots with access levels,
environment variable names (values are never rendered), clock availability,
the no-network/no-subprocess guarantee, the output contract, and a Python stub
block for registered host functions. `MontyPythonCodeTool` appends the snippet
to its LLM-facing description, so the prompt and the in-interpreter behavior
derive from the same configuration and cannot drift.

## `MontyPythonCodeTool`

The agent-facing tool (`monty_python_code`, scope `code:execute`) mirrors
`JavaScriptCodeTool`: error-as-information JSON, camelCase output keys, and a
structured `"rejected"` fallback when the feature is disabled.

```rust
use adk_code::PathAccess;
use adk_tool::MontyPythonCodeTool;
use serde_json::json;
use std::sync::Arc;

let tool = MontyPythonCodeTool::builder()
    .allow_path("/out", "/srv/agent/out", PathAccess::ReadWrite)
    .environ_var("PROJECT", "acme")
    .system_clock()
    .function_fn("get_weather", "Current weather for a city.", |args, _kwargs| async move {
        Ok(json!({ "temp_c": 21 }))
    })
    .build_repl()?;

let agent = LlmAgentBuilder::new("data_agent")
    .instruction("Use monty_python_code for calculations and data work.")
    .model(model)
    .tool(Arc::new(tool))
    .build()?;
```

`MontyPythonCodeTool::new()` builds a fully sandboxed one-shot tool;
`MontyPythonCodeTool::repl()` a fully sandboxed REPL tool.

### Session scoping

In REPL mode, interpreter sessions are keyed by the full ADK session identity
— app name, user id, and session id — so state never leaks between users even
when session id strings repeat across users. All sessions share the same
grants and host-function registry — only interpreter state is per-session.
The session map is bounded by an LRU cap (`max_sessions`, default 100; 0 is
treated as 1); an evicted session's next call transparently starts a fresh
interpreter.

### Tool arguments

| Argument | Type | Description |
|----------|------|-------------|
| `code` | string (required) | Python source to execute |
| `input` | any | Optional JSON value bound to the `input` variable |
| `timeout_secs` | integer | Interpreter time budget (default 30, clamped to 1–300) |
| `reset` | boolean | REPL mode only: discard the persistent session before executing |

### Output envelope

```json
{ "status": "success", "stdout": "", "stderr": "", "output": {"n": 42},
  "stdoutTruncated": false, "stderrTruncated": false, "durationMs": 3 }
```

There is no `exitCode` — execution is in-process, no process is spawned;
`status` is the success/failure signal. `stdoutTruncated` / `stderrTruncated`
report when captured output was cut at the sandbox policy's byte limit (1 MB
each by default).

The value of the script's final expression is returned as `output`; `print()`
output is captured as `stdout`. Failure statuses: `"failed"` (Python
exception — traceback in `stderr`, including exceptions raised by host
functions), `"timeout"` (exceeded time budget), `"rejected"` (bad arguments or
feature disabled). Never a `ToolError`.

The envelope is fixed across both modes, so the tool declares it through
`Tool::response_schema()` — providers that surface response schemas receive it
in the tool declaration alongside `parameters`.

## Relation to CodeAct

The `CodeActAgent` + `adk-codeact-monty` pathway also runs Python via Monty,
but with ADK `Tool` dispatch from inside scripts (`call_tool(...)`) and
suspend/resume across agent turns. `MontyPythonCodeTool` deliberately excludes
both — it is a self-contained code-execution tool whose extensibility seam is
the host-function registry. See [Coding Agent](../coding-agent/index.md) for
CodeAct.

## Example

`examples/monty_python_code_tool` runs an `LlmAgent` with a REPL-mode
`MontyPythonCodeTool` configured with a read-write mount, an environment
variable, and a registered host function — demonstrating multi-turn variable
persistence and host-function calls from model-written Python.
