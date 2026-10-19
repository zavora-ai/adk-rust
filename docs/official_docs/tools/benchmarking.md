# Benchmarking

The `adk-bench` crate and the `cargo adk bench` command benchmark ADK-Rust agents against a live Gemini model. Each model call is timed separately, so framework overhead is measured apart from provider latency.

## What It Measures

| Metric | Description |
|--------|-------------|
| Cold start | Time from launch to the first LLM call |
| Agent loop overhead | Per-turn framework time: total turn time minus the LLM round trip |
| Concurrent throughput | Agents per second under Tokio async load (`--concurrency`, `--sweep`) |
| Memory | Peak RSS during the measurement phase and its growth per concurrent agent, sampled on Linux and macOS |
| Token overhead | Reported by external harnesses only, in their EBP output |
| CV (coefficient of variation) | Stability across runs; a high CV is reported as a warning |

Model calls use `temperature = 0` and a fixed seed (`DeterministicConfig`).

## Quick Start

`cargo adk bench` reads `GOOGLE_API_KEY` and runs every built-in workload against Gemini:

```bash
# Estimate the cost without calling the model
cargo adk bench --dry-run

# Run all built-in workloads with the defaults
cargo adk bench

# Run one workload, by name or by the path of a workload JSON file
cargo adk bench --workload simple_tool_call
```

## CLI Reference

| Flag | Default | Description |
|------|---------|-------------|
| `--model <MODEL>` | `gemini-3.7-flash` | Gemini model ID |
| `--runs <N>` | 5 | Measurement iterations per workload |
| `--warmup <N>` | 3 | Iterations run first and discarded |
| `--concurrency <N>` | 1 | Agents run concurrently |
| `--sweep` | off | Measure concurrency levels 1, 2, 4, 8, 16, 32, 64 |
| `--workload <NAME or PATH>` | all built-in | One workload |
| `--experimental` | off | Also run the experimental `multi_agent_delegation` workload |
| `--format <FORMAT>` | `table` | `table`, `json`, or `markdown` |
| `--output <PATH>` | stdout | Write the formatted results to a file |
| `--dry-run` | off | Log the estimated cost and exit without calling the model |
| `--max-cost-usd <AMOUNT>` | none | Fail when the estimated cost exceeds the amount |
| `--confirm-cost` | off | Required to run when the estimated cost exceeds $1.00 |
| `--save-baseline` | off | Save the results as the regression baseline |
| `--check-regression` | off | Compare the results with the saved baseline |
| `--tolerance <FRACTION>` | `0.10` | Allowed relative degradation (0.10 = 10%) |
| `--external-config <PATH>` | none | External frameworks to run on the same workload (see below) |
| `--external-timeout <SECS>` | 300 | Time limit for each external framework run |

## Cost Control

Benchmarks make real LLM calls. The command estimates the cost of the selected workloads, runs, and
concurrency before it calls the model:

```bash
# Log the estimate without spending anything
cargo adk bench --dry-run

# Fail when the estimate is above a ceiling
cargo adk bench --max-cost-usd 0.50

# Proceed with an estimate above $1.00
cargo adk bench --runs 20 --confirm-cost
```

Without `--confirm-cost`, an estimate above $1.00 fails the command before any model call.

## Regression Detection

```bash
# Save a baseline
cargo adk bench --save-baseline

# On the next change, compare against it
cargo adk bench --check-regression --tolerance 0.10
```

The baseline is `.bench-baseline.json` in the current directory. A timing metric regresses when
`(current - baseline) / baseline` exceeds the tolerance; without a baseline file the check is
skipped.

| Exit code | Meaning |
|-----------|---------|
| `0` | Success; no regression detected |
| `1` | Configuration, run, cost-limit, or output error |
| `2` | At least one metric regressed beyond the tolerance |

## External Framework Comparison (EBP)

The External Benchmark Protocol runs another framework as a subprocess on the same workload and
compares its metrics with ADK-Rust's. `ExternalRunner` sets `BENCH_START_EPOCH_NS`, passes the
workload JSON file as the last argument, and parses the framework's JSON metrics from stdout. A
run past `--external-timeout` fails with `ExternalTimeout`, and the subprocess is killed.

```bash
cargo adk bench --workload simple_tool_call --external-config adk-bench/harnesses/external-frameworks.json
```

The configuration lists each framework's `name`, `command`, `args`, `env`, and `workingDir`.
`adk-bench/harnesses` contains harnesses for the Gemini Python SDK and LangGraph.

## Workloads

| Workload | Exercises |
|----------|-----------|
| `simple_tool_call` | One weather query and one tool call — the closest isolation of framework overhead |
| `multi_step_reasoning` | Sequential tool use where each step depends on the previous: search products, get details, calculate shipping |
| `parallel_tool_invocation` | Stock price, news, and analyst rating fetched concurrently |
| `multi_agent_delegation` (`--experimental`) | A coordinator delegating research and writing to specialist agents |

The multi-step and parallel workloads include simulated tool latency in their overhead
measurement.

## Programmatic Usage

```rust
use adk_bench::{BenchConfig, BenchRunner, OutputFormat, format_result};

let config = BenchConfig {
    model: "gemini-3.5-flash-lite".to_string(),
    runs: 5,
    warmup: 3,
    workload: Some("simple_tool_call".to_string()),
    ..BenchConfig::default()
};

let runner = BenchRunner::new(config);
let results = runner.run().await?;

for result in &results {
    println!("{}", format_result(result, OutputFormat::Table));
}
```

`BenchConfig::default()` uses `gemini-3.5-flash-lite`, 5 runs, 3 warm-up iterations, and
concurrency 1.

## Further Reading

`adk-bench/README.md` records historical results with the model and settings they were measured
under, and describes the EBP output format.

---

**Previous**: [← Code Execution](code-execution.md) | **Next**: [ACP Tools →](acp-tools.md)
