# Multi-Perspective Analysis Example

Three LLM analysts — technical, business, and UX — answer the same question from
their own angle under a single `ParallelAgent`, which runs them concurrently and
merges their event streams.

## What This Shows

- **`ParallelAgent`** — fans one input out to several sub-agents and merges their
  event streams as the events arrive, so wall-clock cost is roughly the slowest
  branch rather than the sum of all branches
- **`Runner::builder()`** — the typestate builder enforces the required fields
  (`app_name`, `agent`, `session_service`) at compile time and does not break when
  optional fields are added, unlike a `RunnerConfig` struct literal
- **Branch attribution** — every streamed chunk is tagged with `event.author`, so
  you can see the three branches interleaving instead of arriving in blocks
- **Measured overlap** — the summary prints wall-clock time next to the sum of
  per-branch spans. Serial execution would make those two numbers roughly equal;
  concurrent execution makes wall clock close to the slowest single branch

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set with a valid Gemini API key

```bash
cp examples/multi_perspective_analysis/.env.example examples/multi_perspective_analysis/.env
# Edit .env and add your GOOGLE_API_KEY
```

## Run

```bash
cargo run --manifest-path examples/multi_perspective_analysis/Cargo.toml
```

## Expected output

Interleaved, branch-tagged chunks followed by a timing summary:

```
Question: Should a startup adopt WebAssembly for their web app?

Running 3 analysts concurrently under ParallelAgent...

[   412 ms] technical_analyst: WebAssembly pays off when you have CPU-bound work ...
[   455 ms] business_analyst: The ROI case depends on whether your bottleneck is ...
[   501 ms] ux_analyst: Users notice WebAssembly only through load time and ...

─── timing ───
  technical_analyst    first event    412 ms, last event    980 ms
  business_analyst     first event    455 ms, last event   1012 ms
  ux_analyst           first event    501 ms, last event    1104 ms
  wall clock            1104 ms
  sum of branch spans   1728 ms

3 analysts finished in 1104 ms. Serial execution would cost roughly the sum above.
```

Exact numbers vary with model latency. The point is that wall clock stays close
to the slowest branch while the sum of branch spans is larger — the branches
overlapped.

> **Isolation:** each analyst runs on its own conversation branch
> (`multi_perspective_analysis.<analyst>`), so a branch does not see what its
> siblings produced — the three opinions are formed independently. The user turn
> and anything produced before the fan-out stay visible to all of them. For
> deliberate cross-agent coordination, use `ParallelAgent::with_shared_state()`
> and the [`parallel_shared_state`](../parallel_shared_state) example.

## Related

| Example | Focus |
|---|---|
| [`parallel_shared_state`](../parallel_shared_state) | Sub-agents coordinating through `SharedState` |
| [`tier_examples`](../tier_examples) | Sequential and graph workflows across feature tiers |
