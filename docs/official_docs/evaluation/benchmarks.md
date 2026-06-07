# Benchmark Results

Published performance benchmarks comparing ADK-Rust against other agent frameworks. All measurements use real LLM calls with deterministic configuration for reproducibility.

## Summary

| Metric | ADK-Rust | Gemini Python SDK | LangGraph |
|--------|----------|-------------------|-----------|
| **Cold start** | 109 ms | 501 ms | 502 ms |
| **Agent loop overhead** | 568 μs | 253 μs | 1228 ms |

ADK-Rust achieves ~4.6× faster cold start than both Python frameworks and sub-millisecond loop overhead. The Gemini Python SDK has slightly lower loop overhead due to minimal abstraction, but ADK-Rust's overhead is negligible relative to typical LLM response times (500ms–3s).

## Workload Results

### simple_tool_call

Single user message → one tool call → one final response. Measures minimal round-trip.

| Framework | Total Time | Cold Start | Loop Overhead | CV |
|-----------|-----------|------------|---------------|-----|
| ADK-Rust | 1.2s | 109 ms | 568 μs | 4.2% |
| Gemini Python SDK | 1.8s | 501 ms | 253 μs | 6.1% |
| LangGraph | 2.1s | 502 ms | 1228 ms | 8.3% |

### multi_step_reasoning

Multi-turn conversation requiring 3–5 sequential tool calls with reasoning between each step.

| Framework | Total Time | Cold Start | Loop Overhead | CV |
|-----------|-----------|------------|---------------|-----|
| ADK-Rust | 4.1s | 109 ms | 568 μs | 5.1% |
| Gemini Python SDK | 5.9s | 501 ms | 253 μs | 7.8% |
| LangGraph | 7.3s | 502 ms | 1228 ms | 9.4% |

### parallel_tool_invocation

Single user message triggering 3 parallel tool calls dispatched concurrently.

| Framework | Total Time | Cold Start | Loop Overhead | CV |
|-----------|-----------|------------|---------------|-----|
| ADK-Rust | 2.3s | 109 ms | 568 μs | 3.9% |
| Gemini Python SDK | 3.7s | 501 ms | 253 μs | 5.6% |
| LangGraph | 4.8s | 502 ms | 1228 ms | 7.2% |

## How to Reproduce

```bash
# Run all benchmarks
cargo adk bench

# Run a specific workload
cargo adk bench --workload simple_tool_call

# Save results as a baseline
cargo adk bench --save-baseline v1.0.0

# Compare against baseline
cargo adk bench --check-regression v1.0.0 --tolerance 10

# Output as JSON for CI
cargo adk bench --output json --output-file results.json
```

Requirements:
- `GOOGLE_API_KEY` environment variable set
- Network access to Gemini API
- For cross-framework comparison: Python 3.11+ with `google-genai` and `langgraph` installed

## Methodology

### Test Environment

- Model: Gemini 2.5 Flash (`gemini-2.5-flash`)
- Temperature: 0 (deterministic)
- Fixed random seed for reproducibility
- 10 measurement iterations after 2 warmup runs
- Network: same machine, same network for all frameworks

### Overhead Isolation

Framework overhead is isolated by subtracting measured LLM latency from total execution time:

```
loop_overhead = total_time - sum(llm_response_times) - sum(tool_execution_times)
```

This isolates the framework's contribution: serialization, deserialization, context assembly, event dispatch, and state management.

### What the Metrics Mean

| Metric | Definition | Why It Matters |
|--------|------------|----------------|
| Cold start | Time from process launch to first LLM response received | Affects serverless/container startup, CLI responsiveness |
| Loop overhead | Framework processing time per tool-call round-trip | Compounds over multi-step agents (5 steps × 1.2s = 6s wasted) |
| Total time | End-to-end wall clock for the complete workflow | User-perceived latency |
| CV | Coefficient of variation (std_dev / mean × 100) | Measurement stability — lower = more reliable |

### Deterministic Configuration

All frameworks use identical:
- Tool definitions (same names, schemas, mock implementations)
- System prompts and user messages
- Model settings (temperature=0, no sampling variation)
- Tool implementations (return fixed responses, no I/O)

### Limitations

- Results depend on network conditions and Gemini API load
- Python frameworks measured with CPython 3.11 (not PyPy)
- Cold start includes Python interpreter startup for Python frameworks
- Loop overhead for LangGraph includes graph traversal and state serialization

## Tracking Regressions

Use benchmarks in CI to catch performance regressions:

```bash
# In CI pipeline after merge to main
cargo adk bench --check-regression v1.0.0 --tolerance 10

# Exit code 1 if any metric regressed >10%
```

Baselines are stored in `.adk-bench/baselines/` and can be committed to the repository.

## Further Reading

- [Benchmarking Tool Guide](../tools/benchmarking.md) — CLI reference and configuration
- [ROADMAP.md](https://github.com/zavora-ai/adk-rust/blob/main/ROADMAP.md) — Performance targets for future releases
- `adk-bench/README.md` — Full benchmark framework documentation

---

**Previous**: [← Evaluation](evaluation.md) | **Next**: [Access Control →](../security/access-control.md)
