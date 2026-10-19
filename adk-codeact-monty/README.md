# adk-codeact-monty

[![Crates.io](https://img.shields.io/crates/v/adk-codeact-monty.svg)](https://crates.io/crates/adk-codeact-monty)
[![Documentation](https://docs.rs/adk-codeact-monty/badge.svg)](https://docs.rs/adk-codeact-monty)
[![License](https://img.shields.io/crates/l/adk-codeact-monty.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

A Python [`CodeRuntime`](https://docs.rs/adk-agent/latest/adk_agent/codeact/trait.CodeRuntime.html) for the ADK-Rust
[`CodeActAgent`](https://docs.rs/adk-agent/latest/adk_agent/codeact/struct.CodeActAgent.html), backed by
[Pydantic Monty](https://github.com/pydantic/monty) — a minimal, secure,
Rust-native Python interpreter built for running LLM-generated code.

With `MontyRuntime`, a `CodeActAgent` *acts by writing Python*: each turn the model
emits a script, invokes your `Tool`s with the built-in
`call_tool("name", {"arg": value})` function, composes their results with real
control flow, and returns a tagged value. Monty runs that
script in-process in microseconds — no container, no subprocess — and can
snapshot a paused run to bytes, which is exactly what the CodeAct suspend/resume
model (HITL confirmation, long-running tools, durable checkpoints) needs.

Operating-system effects a script attempts — filesystem reads/writes,
`os.getenv`/`os.environ`, and `date.today()`/`datetime.now()` — are serviced
in-place against a host-controlled OS-access policy. They are **not** tools and
never pause the agent loop. By default a runtime is fully sandboxed (no
filesystem access, an empty environment), but you can grant a script specific
read-only or read-write paths and an explicit environment map (see
[OS access](#os-access) below).

## Installation

```toml
[dependencies]
adk-agent = { version = "3.0", features = ["codeact"] }
adk-codeact-monty = "3.0"
```

Or use the meta-crate's `codeact-monty` feature, which implies `codeact` and is not part of any
tier:

```toml
[dependencies]
adk-rust = { version = "3.0", features = ["codeact-monty"] }
```

The crate has no feature flags of its own.

> **Note:** this crate is **Experimental** (see [STABILITY.md](https://github.com/zavora-ai/adk-rust/blob/main/STABILITY.md)) —
> the API may change in a minor release; pin an exact version for production use.

## Dependencies

The Monty interpreter is consumed through `adk-code`'s `embedded-python`
integration kernel, which pins the three Monty crates —
[`monty`](https://crates.io/crates/monty) (the interpreter and run/repl
surface), [`monty-types`](https://crates.io/crates/monty-types) (the types
that cross the host boundary — `MontyObject`, `OsFunctionCall`,
`ResourceLimits`, ...), and [`monty-fs`](https://crates.io/crates/monty-fs)
(the boundary-enforcing filesystem `MountTable`) — in exactly one place, and
shares the JSON↔Monty conversion and OS-call servicing with the
`MontyOneShotExecutor`/`MontyReplExecutor` executors, so the two integrations
cannot drift.

## Usage

```rust
use adk_agent::codeact::CodeActAgent;
use adk_codeact_monty::MontyRuntime;
use adk_core::{Llm, Tool};
use std::sync::Arc;

fn python_agent(model: Arc<dyn Llm>, tool: Arc<dyn Tool>) -> adk_core::Result<CodeActAgent> {
    CodeActAgent::builder()
        .name("python_agent")
        .model(model)
        .runtime(Arc::new(MontyRuntime::new()))
        .instruction("Solve the task by writing Python.")
        .tool(tool)
        .build()
}
```

`MontyRuntime::new()` ships with **conservative default resource limits** (5 seconds of
wall-clock time per advance and 256 MiB of memory) so an accidental infinite loop or
runaway allocation in LLM-generated code cannot block the calling task. Limits
ride along inside a serialized continuation, so a resumed run stays bounded too.
Tighten or relax them with the builder:

```rust
use adk_codeact_monty::MontyRuntime;
use std::time::Duration;

fn runtime() -> MontyRuntime {
    MontyRuntime::builder()
        .max_duration(Duration::from_secs(2))
        .max_memory(64 * 1024 * 1024)
        .build()
}
```

For **trusted** scripts only, remove the caps entirely (keeping just Monty's
recursion guard) with `unlimited()`:

```rust
use adk_codeact_monty::MontyRuntime;

fn trusted_runtime() -> MontyRuntime {
    MontyRuntime::builder().unlimited().build()
}
```

Tools are invoked through a single built-in function —
`call_tool("name", {"arg": value, ...})` — and never as bare callables; that is
the *only* way to call a tool. The tool name is a string literal and every
argument is a string-keyed entry in one dict, so:

- the real tool name rides inside the serialized continuation (surviving
  suspend/resume with no host-side name table);
- a tool, *and each argument*, may carry any name — not a valid Python identifier
  (`"fetch-cart"`), a Python keyword, or even `"call_tool"`;
- the driver binds the dict's entries onto the tool's parameters by name
  *exactly*, with no positional-order inference.

Each tool is rendered in the prompt as a `call_tool("name", {...})` usage line
with its parameters and description. Anything but this one form — a bare
`fetch_cart(...)`, keyword arguments to `call_tool`, a non-dict argument, or a
non-string argument key — is refused with a corrective error rather than silently
dispatched.

## Example

A runnable example lives in
[`examples/codeact_monty_agent`](https://github.com/zavora-ai/adk-rust/tree/main/examples/codeact_monty_agent):

```bash
cargo run --manifest-path examples/codeact_monty_agent/Cargo.toml
```

It runs offline with a deterministic model that writes one Python script: it
calls two tools (`fetch_cart`, `tax_rate`) and does real work between them (a
`for` loop, indexing, arithmetic) — two tool calls become two suspend/resume
cycles through Monty.

## How it maps onto the `CodeRuntime` seam

| `CodeRuntime` concept                       | Monty mechanism                                            |
|---------------------------------------------|-----------------------------------------------------------|
| `start(script)`                             | `MontyRun::new(...).start(...)` → `RunProgress`            |
| `RunStep::Call` (tool call)                 | `RunProgress::FunctionCall`                                |
| `PendingCall::positional_args`/`keyword_args` | `FunctionCall.args` / `FunctionCall.kwargs`             |
| `RunStep::Complete`                         | `RunProgress::Complete`                                    |
| `RunStep::Raised` (script/parse error)      | `Err(MontyException)` rendered as a CPython traceback      |
| `RunStep` `stdout`                          | `PrintWriter::CollectString` per advance                  |
| `PendingCall::dump`                         | `RunProgress::dump` (postcard bytes)                       |
| `CodeRuntime::resume(snapshot)`             | `RunProgress::load` → `FunctionCall::resume`              |
| `ResumeWith::Value`                         | `ExtFunctionResult::Return`                                |
| `ResumeWith::Raise`                         | `ExtFunctionResult::Error`                                 |
| `RuntimeCapabilities`                       | `supports_suspension = true` + the Monty language briefing |

Argument binding is **not** the runtime's job: the adapter reports a call's
positional and keyword arguments separately, and the CodeAct driver binds them
onto the tool's parameters via `bind_call_args`. The adapter is therefore
stateless.

Monty's other suspension points are handled in-place by the runtime: OS calls
(filesystem, environment, clock) are serviced against the OS-access policy and
resumed immediately, an undefined-name reference raises `NameError`, and a
blocked `await` is refused to steer the model toward synchronous tool calls.

## OS access

Monty surfaces every operating-system effect a script attempts as an OS call the
host resolves. The runtime services these **in place** — they never become tool
calls and never pause the agent loop — bounded by an `OsAccess` policy:

- **Filesystem.** Only directories you mount with `allow_path` are reachable,
  each read-only or read-write. A script reaches them through `pathlib.Path`
  against the *virtual* mount path; Monty enforces the boundary
  (canonicalization + symlink-escape detection), so a script can never touch a
  host path outside a mount. Access outside every mount raises `PermissionError`
  (existence checks return `False`, matching CPython). Monty implements only a
  subset of `pathlib.Path`, so when paths are mounted the prompt lists the exact
  supported methods — read/query (`exists`, `is_file`, `is_dir`, `is_symlink`,
  `read_text`, `read_bytes`, `stat`, `iterdir`, `resolve`, `absolute`, `open`),
  write (`write_text`, `write_bytes`, `append_text`, `append_bytes`, `mkdir`,
  `unlink`, `rmdir`, `rename`), and pure path ops (`/`, `joinpath`,
  `is_absolute`, `with_name`, `with_stem`, `with_suffix`, `as_posix`, and the
  `name`/`parent`/`stem`/`suffix`/`suffixes`/`parts` properties). Any other
  method raises `AttributeError`.
- **Environment.** `os.getenv(name)` and `os.environ` read the explicit string
  map you supply with `environ` / `environ_var`. Empty by default, so the host
  process environment (and any secrets in it) is never exposed implicitly.
- **Clock.** `date.today()` and `datetime.now()` read the host clock unless you
  disable them with `system_clock(false)`.

Network and subprocess access have no Monty OS-call surface and remain
unavailable regardless of policy.

```rust
use adk_codeact_monty::{MontyRuntime, PathAccess};

fn runtime() -> MontyRuntime {
    MontyRuntime::builder()
        .allow_path("/data", "/srv/agent/data", PathAccess::ReadOnly)
        .allow_path("/out", "/srv/agent/out", PathAccess::ReadWrite)
        .environ_var("PROJECT", "acme")
        .build()
}
```

The granted access is described to the model in the system prompt, so it knows
which paths it may read or write and which environment variables exist.

## Runtime contract

| Concern | Behaviour |
|---------|-----------|
| Arguments | The adapter reports a call's positional and keyword arguments separately (`PendingCall::positional_args`, `keyword_args`); the CodeAct driver binds them onto the tool's parameters with `adk_agent::codeact::bind_call_args`. `call_tool` passes one named dict, so every argument binds by exact name. |
| Errors | Every error the model can fix by writing different code, including a syntax error, is a `RunStep::Raised`; `RuntimeError` is reserved for host failures such as a corrupt snapshot. |
| Output | Each `RunStep` carries the `stdout` the script printed since the previous step; the driver shows it to the model as a `user`-role note and persists it in checkpoints, so output printed before a suspend survives resume. |
| Call ids | Monty numbers calls with `u32`; the adapter widens them to the seam's `u64`. |
| State | The adapter is stateless; `render_tools` is a pure rendering of the tool slice. |

## Safety and tool calling

- **Default resource limits.** `MontyRuntime::new()` applies conservative
  per-advance time and memory caps for untrusted code; `builder().unlimited()`
  opts out for trusted scripts.
- **One way to call a tool.** Every tool is invoked via
  `call_tool("name", {"arg": value})`, so any tool name *and* any argument name is
  safe (hyphens, keywords, even `"call_tool"`), arguments bind by name exactly,
  and any other form — a bare call, keyword arguments, a non-dict argument, or a
  non-string key — is refused with a corrective error instead of being silently
  dispatched. The model has no ambiguous calling form to get wrong. Tool
  descriptions are collapsed to a single comment line in the catalog.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
