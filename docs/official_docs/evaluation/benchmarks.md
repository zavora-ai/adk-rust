# Benchmarks

`cargo adk bench` measures ADK-Rust's framework overhead against a live Gemini model, and can run
other agent frameworks on the same workload for comparison. Results depend on the model, the
network, the machine, and the workload, so this page describes how to produce and read them
rather than publishing fixed numbers. The [Benchmarking](../tools/benchmarking.md) guide is the
full CLI reference.

## Output

The `table` format reports, per workload, the mean, P95, minimum, maximum, and CV of the cold
start and the agent loop overhead, with the ADK, Rust, and OS versions of the run; `json` and
`markdown` carry the same measurements.

## How to Reproduce

```bash
# Estimate the cost without calling the model
cargo adk bench --dry-run

# Run all built-in workloads
cargo adk bench

# Run a specific workload
cargo adk bench --workload simple_tool_call

# Save results as the regression baseline (.bench-baseline.json)
cargo adk bench --save-baseline

# Compare against the baseline; 0.10 allows a 10% relative degradation
cargo adk bench --check-regression --tolerance 0.10

# Output as JSON for CI
cargo adk bench --format json --output results.json

# Run the Gemini Python SDK and LangGraph harnesses on the same workload
cargo adk bench --workload simple_tool_call --external-config adk-bench/harnesses/external-frameworks.json
```

Requirements:
- `GOOGLE_API_KEY` environment variable set
- Network access to the Gemini API
- For cross-framework comparison: Python 3 with the packages the harnesses in
  `adk-bench/harnesses` import (`google-generativeai`, `langgraph`, `langchain-google-genai`,
  `langchain-core`), run from the repository root

## Methodology

### Test Environment

- Model: `--model`, default `gemini-3.7-flash`
- Model calls use temperature 0, `top_p` 1.0, and seed 42 (`DeterministicConfig`)
- `--runs` measurement iterations (default 5) after `--warmup` iterations (default 3), which
  are discarded

### Overhead Isolation

Each model call is timed by an instrumented model wrapper. Framework overhead is the turn time
minus the time spent in model calls:

```
loop_overhead = total_turn_time - sum(llm_round_trip_times)
```

Tool execution, including the simulated latency of a workload's tools, is part of the overhead.

### What the Metrics Mean

| Metric | Definition | Why It Matters |
|--------|------------|----------------|
| Cold start | Time from the start of the run to the first model call | Affects serverless/container startup, CLI responsiveness |
| Loop overhead | Turn time not spent in model calls, per turn | Compounds over multi-step agents |
| CV | Coefficient of variation (std_dev / mean × 100) | Measurement stability — lower = more reliable; a high CV is logged as a warning |

### Deterministic Configuration

The built-in workloads define their tools with fixed responses and simulated latency, so a run
does no tool I/O. The external harnesses receive the same workload JSON file.

### Limitations

- Results depend on network conditions and Gemini API load
- Cold start for Python frameworks includes interpreter startup
- Free-tier Gemini quotas limit how many runs complete in a minute

## Tracking Regressions

Use benchmarks in CI to catch performance regressions:

```bash
# In a CI pipeline, after restoring .bench-baseline.json
cargo adk bench --check-regression --tolerance 0.10

# Exit code 2 if any metric regressed by more than 10%
```

The baseline is `.bench-baseline.json` in the current directory and can be committed to the
repository. Without a baseline file the check is skipped.

## Further Reading

- [Benchmarking Tool Guide](../tools/benchmarking.md) — CLI reference and configuration
- [ROADMAP.md](https://github.com/zavora-ai/adk-rust/blob/main/ROADMAP.md) — Performance targets for future releases
- `adk-bench/README.md` — Full benchmark framework documentation

---

**Previous**: [← Evaluation](evaluation.md) | **Next**: [Access Control →](../security/access-control.md)
