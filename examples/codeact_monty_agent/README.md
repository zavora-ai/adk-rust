# CodeAct × Monty agent example

Runs the ADK-Rust [`CodeActAgent`](../../adk-agent/src/codeact) against a **real
Python interpreter** via [`adk-codeact-monty`](../../adk-codeact-monty) — the
`CodeRuntime` backed by [Pydantic Monty](https://github.com/pydantic/monty).

Where the sibling [`codeact_agent`](../codeact_agent) example uses a toy
line-script runtime, this one runs genuine Python: the model writes a script that
reads the **environment** with `os.getenv`, stamps the result with the **clock**
(`datetime.now()`), invokes tools via `call_tool("name", {"arg": value})`, *and*
does real work between them (a `for` loop, indexing, arithmetic). It is fully
self-contained — a deterministic model (`DemoLlm`) emits the script, so it runs
with **no API key**.

## Run

Run from inside this directory so the local `rust-toolchain.toml` (rustc 1.95+,
required by Monty) is selected automatically:

```bash
cd examples/codeact_monty_agent
cargo run
```

> This example transitively depends on `monty` (a git dependency, not yet on
> crates.io), which requires **rustc 1.95+**. The bundled `rust-toolchain.toml`
> selects it automatically; the first build also fetches and compiles Monty.

Expected output (the `priced_at` timestamp reflects the host clock at run time):

```
=== ADK-Rust CodeAct × Monty (Python) example ===

[cart_assistant]
{
  "lines": 3,
  "priced_at": "2026-06-23T18:08:54.935552",
  "region": "CA",
  "subtotal": 132.0,
  "tax_rate": 0.0725,
  "total": 141.57,
  "user": "u-42"
}

Done.
```

## What it shows

The model writes **one** Python script that:

1. reads `CART_USER` and `TAX_REGION` from the environment with `os.getenv`,
2. calls the `fetch_cart` tool to load that user's cart,
3. loops in Python to sum line items,
4. calls the `tax_rate` tool and applies it with ordinary arithmetic, and
5. stamps the result with `datetime.now()` and returns a tagged `final_result`.

### OS functions vs. tools

The environment (`os.getenv` / `os.environ`) and clock (`datetime.now()` /
`date.today()`) are **OS functions**: the host services them *in place* against
the policy configured on the `MontyRuntime` builder —

```rust
let runtime = MontyRuntime::builder()
    .environ_var("CART_USER", "u-42")
    .environ_var("TAX_REGION", "CA")
    .system_clock(true) // the builder default; shown for clarity
    .build();
```

— so they never become tools and never pause the agent loop. The default policy
is fully sandboxed (empty environment, no filesystem, host clock enabled); this
example grants an explicit two-variable environment and leaves the clock on.

The two `call_tool` invocations, by contrast, become two suspend/resume cycles in
Monty: the interpreter pauses at each call boundary, the agent runs the tool, and
execution resumes exactly where it left off — the same snapshot-at-call-boundary
mechanism that powers HITL confirmation, long-running tools, and durable
checkpoints.

## The pieces

- **`src/main.rs`** — `DemoLlm` (a deterministic model), the `fetch_cart` and
  `tax_rate` tools, and the `CodeActAgent` + `MontyRuntime` + `Runner` wiring.
- **[`adk-codeact-monty`](../../adk-codeact-monty)** — the reusable Python
  `CodeRuntime` this example depends on.

## Going to production

Swap one piece; the rest is unchanged: replace `DemoLlm` with an
[`adk-model`](../../adk-model) provider (Gemini, OpenAI, Anthropic, ...) and let a
real LLM write the Python.
