# adk-codeact-monty

A Python [`CodeRuntime`](../adk-agent/src/codeact/runtime.rs) for the ADK-Rust
[`CodeActAgent`](../adk-agent/src/codeact), backed by
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

## Why a separate crate (outside the workspace)?

Monty is not on crates.io yet, so it is pulled in as a **git dependency** pinned
to the `v0.0.18` release commit. A published workspace can't carry a git
dependency, so this crate is its own workspace (note the empty `[workspace]`
table in `Cargo.toml`) and is excluded from the root `adk-rust` workspace.

## Usage

```rust,no_run
use std::sync::Arc;
use adk_agent::codeact::CodeActAgent;
use adk_codeact_monty::MontyRuntime;

let agent = CodeActAgent::builder()
    .name("python_agent")
    .model(model)
    .runtime(Arc::new(MontyRuntime::new()))
    .instruction("Solve the task by writing Python.")
    .tool(Arc::new(MyTool))
    .build()?;
```

`MontyRuntime::new()` ships with **conservative default resource limits** (a
per-advance wall-clock cap and a memory cap) so an accidental infinite loop or
runaway allocation in LLM-generated code cannot block the calling task. Limits
ride along inside a serialized continuation, so a resumed run stays bounded too.
Tighten or relax them with the builder:

```rust
use std::time::Duration;
use adk_codeact_monty::MontyRuntime;

let runtime = MontyRuntime::builder()
    .max_duration(Duration::from_secs(2))
    .max_memory(64 * 1024 * 1024)
    .build();
```

For **trusted** scripts only, remove the caps entirely (keeping just Monty's
recursion guard) with `unlimited()`:

```rust
use adk_codeact_monty::MontyRuntime;

let runtime = MontyRuntime::builder().unlimited().build();
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
[`examples/codeact_monty_agent`](../examples/codeact_monty_agent) (run it from
that directory so its `rust-toolchain.toml` selects rustc 1.95+):

```bash
cd examples/codeact_monty_agent && cargo run
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

let runtime = MontyRuntime::builder()
    .allow_path("/data", "/srv/agent/data", PathAccess::ReadOnly)
    .allow_path("/out", "/srv/agent/out", PathAccess::ReadWrite)
    .environ_var("PROJECT", "acme")
    .build();
```

The granted access is described to the model in the system prompt, so it knows
which paths it may read or write and which environment variables exist.

## Notes on the CodeActAgent API (dogfooding feedback)

Building this adapter surfaced several rough edges in the `CodeRuntime` seam.
Four of the five have since been **fixed in `adk-agent`** (the fifth was left as
is on purpose); this section records the original friction and what changed.

1. **(fixed) Arguments are now positional + keyword, bound centrally.**
   Originally `PendingCall::args()` was a single flattened JSON object, forcing a
   runtime that gets positional *and* keyword arguments (like Monty's
   `args`/`kwargs`) to map positional args back onto parameter names — with *no
   schema at the call site*. The only workaround was to scrape schemas out of
   `render_tools` and cache them in interior-mutable state. The seam now exposes
   `PendingCall::positional_args()` and `keyword_args()` separately, and the
   driver binds them onto the tool's parameters in one place
   (`adk_agent::codeact::bind_call_args`). This adapter is now **stateless** — no
   lock, no cached map.

2. **(fixed) `render_tools` is now a pure rendering hook.** It used to be the
   only place a runtime saw the tool set, so it doubled as "describe the tools"
   and "remember them for arg-binding," which is what drove the shared mutable
   state above. With central binding, a runtime never needs to remember anything
   from it: `render_tools` is documented and used as a pure function of the tool
   slice. (No signature change was required — removing the *reason* to cache was
   the fix.)

3. **(fixed) Script vs. host errors are now unambiguous.** Previously a runtime
   had to split errors between `RunStep::Raised` (model's mistake → feed back)
   and `RuntimeError` (host failure → abort), but the `RuntimeError::Parse`
   variant blurred the line: a parse failure is a *model* mistake yet was modeled
   as a host error. `RuntimeError::Parse` has been removed; **all** script-visible
   errors — including syntax/parse failures — now flow through `RunStep::Raised`,
   and `RuntimeError` is strictly host failure (snapshot/internal). The rustdoc
   states the rule plainly: *if the model could fix it by writing different code,
   it's `RunStep::Raised`.*

4. **(left as-is, by request) `call_id` is `u64` on the seam but `u32` in Monty.**
   A one-line `u64::from(...)` at the boundary; not worth a breaking change.

5. **(fixed) `stdout` now has a channel.** Each `RunStep` carries the `stdout`
   the script produced since the previous step (`RunStep::with_stdout`). This
   adapter captures `print()` output with `PrintWriter::CollectString` and
   attaches it; the driver surfaces it back to the model so it can see what its
   code printed.

### Re-evaluation after the changes

The adapter shrank and lost all interior mutability. A couple of smaller things
remain, none blocking:

- **Argument binding is exact for this adapter.** Monty passes arguments as a
  single named dict, so they reach `bind_call_args` as exact name→value pairs
  with no positional-order inference. The framework's positional heuristic
  (`required` first, then `properties`) only matters for runtimes that surface
  positional arguments; this one never does.
- **stdout is surfaced as a `user`-role transcript note** and persisted into
  checkpoints so output printed before a suspend survives resume and crash
  recovery. An agent that wants to route it elsewhere (a trace span, a UI
  channel) can still consume the `stdout` field on each `RunStep` itself.

### Safety and tool calling

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
