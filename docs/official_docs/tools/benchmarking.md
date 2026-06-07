# Benchmarking

The `adk-bench` crate and `cargo adk bench` command provide real-LLM benchmarking for ADK-Rust agents. Unlike synthetic microbenchmarks, `adk-bench` measures actual end-to-end performance with live model calls, isolating framework overhead from provider latency.

## What It Measures

| Metric | Description |
|--------|-------------|
| Cold start | Time from process launch to first LLM response |
| Agent loop overhead | Framework cost per tool-call round-trip (excludes LLM wait time) |
| Throughput | Requests per second under sustained load |
| Memory | Peak RSS during execution |
| Token overhead | Extra tokens added by framework instrumentation |
| CV (Coefficient of Variation) | Stability of measurements across runs |

## Quick Start

```bash
# Run all benchmarks with default settings
cargo adk bench

# Run a specific workload
cargo adk bench --workload simple_tool_call

# Dry run (no LLM calls, validates config only)
cargo adk bench --dry-run
```

## CLI Reference

```bash
cargo adk bench [OPTIONS]

OPTIONS:
    --workload <NAME>          Run a specific workload (simple_tool_call,
                               multi_step_reasoning, parallel_tool_invocation)
    --iterations <N>           Number of iterations per workload [default: 10]
    --warmup <N>               Warmup iterations before measurement [default: 2]
    --provider <NAME>          LLM provider to benchmark [default: gemini]
    --model <MODEL>            Model ID to use [default: gemini-2.5-flash]
    --output <FORMAT>          Output format: table, json, csv [default: table]
    --output-file <PATH>       Write results to file instead of stdout

    # Cost control
    --dry-run                  Validate configuration without making LLM calls
    --max-cost-usd <AMOUNT>    Abort if estimated cost exceeds this amount
    --confirm-cost             Prompt for confirmation before running

    # Regression detection
    --save-baseline <NAME>     Save results as a named baseline
    --check-regression <NAME>  Compare against a saved baseline
    --tolerance <PERCENT>      Regression threshold percentage [default: 10]

    # External comparison
    --ebp                      Enable External Benchmark Protocol output
    --harness <PATH>           Path to external framework harness config
```

## Cost Control

Benchmarks make real LLM calls. Use these flags to avoid unexpected bills:

```bash
# Preview what would run without spending anything
cargo adk bench --dry-run

# Set a hard cost ceiling
cargo adk bench --max-cost-usd 5.00

# Require manual confirmation after cost estimate
cargo adk bench --confirm-cost
```

The cost estimator uses token counts from previous runs (or estimates from the workload definition) multiplied by the provider's published per-token pricing.

## Regression Detection

Track performance across releases:

```bash
# Establish a baseline after a release
cargo adk bench --save-baseline v1.0.0

# On the next change, check for regressions
cargo adk bench --check-regression v1.0.0 --tolerance 10

# Tighter tolerance for critical paths
cargo adk bench --workload simple_tool_call \
    --check-regression v1.0.0 --tolerance 5
```

Exit codes:
- `0` — no regressions detected
- `1` — at least one metric regressed beyond tolerance
- `2` — configuration or runtime error

Baselines are stored in `.adk-bench/baselines/` as JSON files.

## External Framework Comparison (EBP)

The External Benchmark Protocol (EBP) enables apples-to-apples comparison with other agent frameworks. EBP defines a standard workload format and measurement protocol so results are comparable across implementations.

```bash
# Output EBP-compatible results
cargo adk bench --ebp --output json > results.json

# Run against an external harness (e.g., LangGraph, Python SDK)
cargo adk bench --harness harnesses/langraph.toml
```

Harness configuration files define how to invoke external frameworks with the same workloads and measure their results using the same methodology.

## Benchmark Results

Published results comparing ADK-Rust against other frameworks (deterministic config, same model, same workload):

| Metric | ADK-Rust | Gemini Python SDK | LangGraph |
|--------|----------|-------------------|-----------|
| **Cold start** | 109 ms | 501 ms | 502 ms |
| **Loop overhead** | 568 μs | 253 μs | 1228 ms |
| **simple_tool_call** | 1.2s total | 1.8s total | 2.1s total |
| **multi_step_reasoning** | 4.1s total | 5.9s total | 7.3s total |
| **parallel_tool_invocation** | 2.3s total | 3.7s total | 4.8s total |

**Methodology:**
- Real Gemini 2.5 Flash calls (not mocked)
- Deterministic config: `temperature=0`, fixed seed
- 10 iterations after 2 warmup runs
- Overhead isolation: total time minus measured LLM latency
- Same tool definitions and prompts across all frameworks

## Workloads

### simple_tool_call

Single user message → one tool call → one response. Measures the minimal round-trip overhead.

### multi_step_reasoning

Multi-turn conversation requiring 3–5 sequential tool calls with reasoning between each. Measures sustained loop performance.

### parallel_tool_invocation

Single user message triggering 3 parallel tool calls. Measures concurrent dispatch overhead.

## Programmatic Usage

```rust
use adk_bench::{BenchmarkSuite, BenchConfig, Workload};

let config = BenchConfig::builder()
    .iterations(10)
    .warmup(2)
    .provider("gemini")
    .model("gemini-2.5-flash")
    .build()?;

let suite = BenchmarkSuite::new(config);
let results = suite.run_all().await?;

for result in &results {
    println!("{}: cold_start={}ms overhead={}μs",
        result.workload,
        result.cold_start_ms,
        result.loop_overhead_us,
    );
}
```

## Further Reading

See `adk-bench/README.md` for:
- Custom workload definitions
- Harness authoring guide
- CI integration patterns
- Historical result tracking

---

**Previous**: [← Code Execution](code-execution.md) | **Next**: [ACP Tools →](acp-tools.md)
