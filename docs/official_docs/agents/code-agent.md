# CodeActAgent (CodeAct)

`CodeActAgent` is a peer to [`LlmAgent`](llm-agent.md) that **acts by writing and
running code** instead of emitting one tool call at a time. Each turn the model
produces a single script; tools are exposed as callable functions the script can
compose; and the script communicates its result by returning a tagged value.

This is the *CodeAct* pattern: rather than `call tool A → observe → call tool B`,
the model writes `b(a(x))` in one script, so multi-step work happens in a single
turn. It is enabled by the `codeact` feature on `adk-agent`.

## When to use it

- Tasks that chain or combine several tools per turn (data wrangling, batch
  operations, glue logic).
- Models post-trained for code generation.
- Workflows where a real interpreter (e.g. Python) is available as the action
  substrate.

For native tool-calling, prefer [`LlmAgent`](llm-agent.md). For a sandboxed
file/shell coding harness, see the [Coding Agent](../coding-agent/index.md).

## How the loop works

Each turn:

1. The model emits one fenced code block (a script).
2. The script runs on a [`CodeRuntime`]; tool calls surface to the host, which
   executes the tool and resumes the script with the result.
3. The script returns a tagged `ScriptOutput`:
   - `observation` — fed back to the model; the loop continues.
   - `error` — fed back as a message; the loop continues.
   - `final_result` — returned to the caller; the loop ends.
   - `transfer_to_agent` — hands control to another agent; the loop ends.

The framework is **language-agnostic**: the `CodeRuntime` trait is the step-wise
interpreter seam, and it reports its own language/environment to the model via a
freeform prompt. The intended production adapter wraps
[Monty](https://github.com/pydantic/monty), a Rust-native Python interpreter.

## Durability: suspend and resume

`CodeActAgent` is stateless across invocations — durable state lives in the
**session**, exactly like `LlmAgent`. Two situations *suspend* the run:

- a confirmation-gated tool with no decision yet (HITL), and
- a long-running tool whose result arrives out-of-band.

On suspension, the live interpreter continuation is serialized into a
`CodeActCheckpoint` and written to session state; the next `run()` reads it back
and resumes — the confirmation decision arrives via
`RunConfig::tool_confirmation_decisions`, and a long-running result arrives as a
`FunctionResponse` in the next message. Inline tool calls are bracketed with
write-ahead (SAVE-BEFORE) and SAVE-AFTER checkpoints: once the SAVE-AFTER
checkpoint is persisted, recovery resumes with the stored result and never
re-runs the tool. A crash in the narrow window after a tool's side effect but
before its SAVE-AFTER checkpoint lands will re-run the tool on recovery, so
tools that are not idempotent should guard against that (the same at-least-once
boundary as `LlmAgent`).

This requires a runtime that can snapshot a paused call. A runtime that cannot
runs long-running tools inline and rejects confirmation pauses.

## Building a CodeActAgent

```rust,ignore
use adk_agent::codeact::CodeActAgent;
use std::sync::Arc;

// `model` implements `adk_core::Llm`; `runtime` implements `CodeRuntime`.
let agent = CodeActAgent::builder()
    .name("analyst")
    .model(model)
    .runtime(runtime)
    .instruction("Prefer concise, composable steps.")
    .tool(Arc::new(load_csv_tool))
    .output_key("report")
    .build()?;
```

`model` and `runtime` are required; everything else has a default.

## Parity with LlmAgent

The builder mirrors `LlmAgentBuilder`:

- **Model**: `generate_content_config` plus `temperature`/`top_p`/`top_k`/
  `max_output_tokens` shorthands.
- **Instructions**: `instruction`/`instruction_provider`,
  `global_instruction`/`global_instruction_provider`, with `{state.key}`
  template injection; plus skills (`skills` feature).
- **History**: `include_contents`.
- **Tools**: static `tool`s and per-invocation `toolset`s; `tool_timeout`,
  `default_retry_budget`/`tool_retry_budget`, `circuit_breaker_threshold`, and
  `on_tool_error` fallbacks.
- **Authorization**: `ToolConfirmationPolicy`
  (`require_tool_confirmation`/`require_tool_confirmation_for_all`).
- **Transfer**: `sub_agent`s and `disallow_transfer_to_parent`/
  `disallow_transfer_to_peers`.
- **Output**: `output_key`, `output_schema`/`output_type` with a
  correction-retry loop (`output_max_retries`).
- **Callbacks**: `before_callback`/`after_callback`,
  `before_model_callback`/`after_model_callback`, and
  `before_tool_callback`/`after_tool_callback`/`after_tool_callback_full`.
  After-tool callbacks can inspect structured execution metadata via
  `CallbackContext::tool_outcome()`.
- **Feature-gated**: input/output guardrails (`guardrails`) and the
  `EnhancedPlugin` pipeline (`enhanced-plugins`).

Each tool call gets a fresh `ToolContext` that carries the interpreter call id
and delegates artifacts, memory, shared state, user scopes, and secrets to the
live invocation — so a tool behaves identically under `CodeActAgent` or `LlmAgent`.

### Deliberate differences

- Code-execution sandboxing is the `CodeRuntime`'s responsibility, not a
  bolt-on.
- Tool dispatch is **sequential by design** (a single continuation is
  snapshotted at one call boundary), so there is no parallel
  `tool_execution_strategy`.
- There is no `skip_summarization` builder option — the model ends the loop
  itself via `final_result` — though a tool that sets `skip_summarization` on its
  actions still ends the run.

## Example

A runnable, dependency-free end-to-end demo — a self-contained `CodeRuntime` plus
a deterministic model — lives in
[`examples/codeact_agent`](https://github.com/zavora-ai/adk-rust/tree/main/examples/codeact_agent):

```bash
cargo run --manifest-path examples/codeact_agent/Cargo.toml
```

## Implementing a CodeRuntime

A `CodeRuntime` parses and steps a script, surfacing one external call at a time:

```rust,ignore
pub trait CodeRuntime: Send + Sync {
    fn start(&self, script: &str, script_name: &str) -> Result<RunStep, RuntimeError>;
    fn resume(&self, snapshot: &[u8], with: ResumeWith) -> Result<RunStep, RuntimeError>;
    fn capabilities(&self) -> RuntimeCapabilities { /* default */ }
    fn render_tools(&self, tools: &[Arc<dyn Tool>]) -> String { /* default */ }
}
```

- `RunStep` is a set of struct variants — `Call { call, stdout }`,
  `Complete { value, stdout }`, and `Raised { message, stdout }`. Construct them
  with the `RunStep::call` / `RunStep::complete` / `RunStep::raised` helpers and
  attach captured output with `.with_stdout(..)`. `RunStep::Call` surfaces
  exactly one pending call; resume it with a value or an error, or `dump()` its
  continuation to suspend. The `stdout` a runtime attaches is surfaced back to
  the model and persisted into checkpoints, so it survives suspend/resume.
- A `PendingCall` reports its arguments the way the interpreter produced them —
  `positional_args()` and `keyword_args()` separately. **Do not** map positional
  arguments onto names yourself: the driver binds them onto the tool's parameters
  centrally via `adk_agent::codeact::bind_call_args`, so a runtime needs no tool
  schema at the call boundary and `render_tools` can be a pure function of the
  tool slice.
- **Script vs. host errors.** Anything the model could fix by writing different
  code — a syntax/parse error, an uncaught exception, a resource-limit
  cancellation — is a `RunStep::Raised` (an opaque string fed back to the model
  verbatim). `RuntimeError` is reserved for genuine host failures (snapshot
  (de)serialization, internal interpreter errors) and aborts the run.
- `RuntimeCapabilities::supports_suspension` must be `true` to enable HITL and
  long-running deferral; `prompt` describes the language/environment to the model.

See `examples/codeact_agent/src/runtime.rs` for a complete, minimal
implementation that supports suspend/resume.

## Python via Monty

The intended production adapter is
[`adk-codeact-monty`](https://github.com/zavora-ai/adk-rust/tree/main/adk-codeact-monty),
a `CodeRuntime` backed by [Pydantic Monty](https://github.com/pydantic/monty). It
lets the model *act by writing Python*, runs in-process with no container or
subprocess, and snapshots a paused run to bytes — exactly what suspend/resume
needs. It is kept outside the workspace because Monty is currently a git
dependency (not yet on crates.io) and requires rustc 1.95+.

```rust,ignore
use adk_codeact_monty::MontyRuntime;

// Conservative default resource limits (per-advance time + memory caps) make
// `new()` safe for untrusted, LLM-generated code.
let runtime = Arc::new(MontyRuntime::new());

// Tighten or relax with the builder; `unlimited()` removes the caps for
// trusted scripts only.
let runtime = Arc::new(
    MontyRuntime::builder()
        .max_duration(std::time::Duration::from_secs(2))
        .max_memory(64 * 1024 * 1024)
        .build(),
);
```

### OS access

Operating-system effects a script attempts — filesystem reads/writes,
`os.getenv`/`os.environ`, and `date.today()`/`datetime.now()` — are serviced
*in place* against a host-controlled policy. They are **not** tools and never
pause the agent loop. By default a runtime is fully sandboxed (no filesystem
access, an empty environment, host clock enabled). Grant specific access with
the builder:

```rust,ignore
use adk_codeact_monty::{MontyRuntime, PathAccess};

let runtime = Arc::new(
    MontyRuntime::builder()
        // Mount host directories at virtual paths; Monty enforces the boundary
        // (canonicalization + symlink-escape detection) so a script can never
        // escape a mount. Reads/writes outside every mount raise PermissionError.
        .allow_path("/data", "/srv/agent/data", PathAccess::ReadOnly)
        .allow_path("/out", "/srv/agent/out", PathAccess::ReadWrite)
        // Expose an explicit environment map to os.getenv / os.environ. Empty by
        // default — the host process environment is never exposed implicitly.
        .environ_var("PROJECT", "acme")
        // date.today() / datetime.now() read the host clock (enabled by default).
        .system_clock(true)
        .build(),
);
```

Network and subprocess access have no Monty OS-call surface and remain
unavailable regardless of policy. The granted access is described to the model
in the system prompt, so it knows which paths it may read or write and which
environment variables exist.

Monty implements only a subset of `pathlib.Path`, so when paths are mounted the
prompt lists the exact supported methods (any other raises `AttributeError`):

- **Read/query** (any mount): `exists()`, `is_file()`, `is_dir()`,
  `is_symlink()`, `read_text()`, `read_bytes()`, `stat()`, `iterdir()`,
  `resolve()`, `absolute()`, `open("r")`.
- **Write** (read-write mounts only): `write_text()`, `write_bytes()`,
  `append_text()`, `append_bytes()`, `mkdir()`, `unlink()`, `rmdir()`,
  `rename()`, `open("w")`/`open("a")`.
- **Pure path ops** (no I/O): the `/` operator and `joinpath()`,
  `is_absolute()`, `with_name()`, `with_stem()`, `with_suffix()`, `as_posix()`,
  and the `.name`, `.parent`, `.stem`, `.suffix`, `.suffixes`, `.parts`
  properties.

Tools are invoked through a single built-in function,
`call_tool("name", {"arg": value, ...})` — the only way to call a tool; they are
never in scope as bare callables. The tool name is a string literal and every
argument is a string-keyed entry in one dict, so the real name travels inside the
serialized continuation (surviving suspend/resume with no host-side name table),
a tool *and each argument* may carry any name (not a valid Python identifier like
`"fetch-cart"`, a Python keyword, or even `"call_tool"`), and the driver binds the
dict's entries by name exactly — no positional inference. Each tool appears in the
prompt as a `call_tool("name", {...})` usage line with its parameters and
description. Anything but this one form — a bare `fetch_cart(...)`, keyword
arguments, a non-dict argument, or a non-string key — is refused with a corrective
error rather than silently dispatched, so the model has exactly one calling form
to learn.

The runnable
[`examples/codeact_monty_agent`](https://github.com/zavora-ai/adk-rust/tree/main/examples/codeact_monty_agent)
drives a `CodeActAgent` against real Python entirely offline.
