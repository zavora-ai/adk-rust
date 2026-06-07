# adk-bench

A comprehensive benchmarking framework for ADK-Rust that measures framework-level runtime performance using real LLM APIs and supports cross-framework comparison.

## Results

### Framework Comparison: simple_tool_call

All frameworks execute the same workload (weather tool call) against `gemini-2.5-flash` with identical prompts and deterministic config (temperature=0).

| Framework | Cold Start | Agent Loop Overhead (mean) | Agent Loop Overhead (P95) | Peak RSS |
|-----------|-----------|---------------------------|--------------------------|----------|
| **ADK-Rust** | **109 ms** | **568 μs** | **615 μs** | ~15 MB |
| Gemini Python SDK | 501 ms | 253 μs | 334 μs | 69.7 MB |
| LangGraph | 502 ms | 1,228 ms | 1,228 ms | 92.7 MB |

### Full ADK-Rust Suite (3 runs, 1 warmup)

| Workload | Cold Start (mean) | Loop Overhead (mean) | Loop Overhead (P95) | CV |
|----------|------------------|---------------------|--------------------|----|
| simple_tool_call | 117 ms | 368 μs | 475 μs | 20.6% |
| multi_step_reasoning | 129 ms | 38.6 ms | 56.4 ms | 41.3% |
| parallel_tool_invocation | 117 ms | 159.6 ms | 215.5 ms | 29.2% |

> **Note:** Multi-step and parallel workloads include simulated tool latency (10-25ms per tool call) in the overhead measurement. The simple_tool_call workload best isolates pure framework overhead.

### Key Takeaways

- **4.6× faster cold start** — Rust binary startup vs Python interpreter (109ms vs 501ms)
- **Sub-millisecond framework overhead** — ADK-Rust adds ~568μs per agent turn beyond LLM latency
- **4–6× lower memory** — ~15MB RSS vs 70-93MB for Python frameworks
- **Deterministic measurement** — temperature=0, fixed seed, structured output for reproducibility

## How It Works

```
┌─────────────────────────────────────────────────────────────────┐
│ cargo adk bench                                                  │
├─────────────────────────────────────────────────────────────────┤
│                                                                   │
│  BenchRunner                                                      │
│  ├── Warm-up phase (iterations discarded)                        │
│  ├── Measurement phase                                           │
│  │   ├── InstrumentedLlm → Real API (temp=0, seed=42)          │
│  │   ├── Record: request_sent → response_complete                │
│  │   └── Overhead = total_turn - llm_round_trip                  │
│  ├── Concurrency sweep (1, 2, 4, 8, 16, 32, 64)                │
│  ├── Memory sampling (RSS via platform APIs)                     │
│  └── Regression detection (baseline compare)                     │
│                                                                   │
│  ExternalRunner (EBP Protocol)                                    │
│  ├── Spawn subprocess with BENCH_START_EPOCH_NS                  │
│  ├── Pass workload JSON as last arg                              │
│  ├── Parse EBP JSON from stdout                                  │
│  └── Compute cold_start from external clock                      │
│                                                                   │
└─────────────────────────────────────────────────────────────────┘
```

### Metrics Collected

| Metric | Description |
|--------|-------------|
| **Cold Start** | Process launch → first LLM API call |
| **Agent Loop Overhead** | Per-turn framework processing time (total turn minus LLM round-trip) |
| **Concurrent Throughput** | Agents completed per second at N concurrency |
| **Memory Footprint** | Peak RSS via `/proc/self/statm` (Linux) or `mach_task_basic_info` (macOS) |
| **Tool Invocation Latency** | Deserialization + schema validation + execution dispatch |
| **Token Overhead** | Framework-injected tokens beyond user content |

## Usage

### Basic Run

```bash
# Run all built-in workloads with default settings
cargo adk bench --confirm-cost

# Single workload, minimal cost
cargo adk bench --workload simple_tool_call --runs 2 --warmup 1 --confirm-cost
```

### Cost Control

```bash
# See estimated cost without making API calls
cargo adk bench --dry-run

# Set a hard cost limit
cargo adk bench --max-cost-usd 5.00 --confirm-cost

# Minimal run for quick validation
cargo adk bench --workload simple_tool_call --runs 1 --warmup 0 --confirm-cost
```

### Framework Comparison

```bash
# Compare ADK-Rust against Python frameworks
cargo adk bench --workload simple_tool_call --runs 3 \
  --external-config adk-bench/harnesses/external-frameworks.json \
  --format markdown --confirm-cost
```

### Concurrency Sweep

```bash
# Test throughput at multiple concurrency levels
cargo adk bench --workload simple_tool_call --sweep --runs 3 --confirm-cost
```

### Regression Detection (CI)

```bash
# Save a baseline
cargo adk bench --save-baseline --confirm-cost

# Check for regressions (exit code 2 if regressed)
cargo adk bench --check-regression --tolerance 0.10 --confirm-cost
```

### Output Formats

```bash
# JSON (machine-readable, all raw metrics)
cargo adk bench --format json --output results.json --confirm-cost

# Markdown (README-ready comparison table)
cargo adk bench --format markdown --confirm-cost

# Table (terminal display, default)
cargo adk bench --format table --confirm-cost
```

## CLI Reference

| Flag | Default | Description |
|------|---------|-------------|
| `--model` | `gemini-2.5-flash` | LLM model identifier |
| `--runs` | `5` | Measurement iterations per workload |
| `--warmup` | `3` | Warm-up iterations (discarded) |
| `--concurrency` | `1` | Agent concurrency level |
| `--workload` | all built-in | Specific workload name or file path |
| `--format` | `table` | Output format: `table`, `json`, `markdown` |
| `--output` | stdout | Write results to file |
| `--sweep` | off | Test concurrency levels 1,2,4,8,16,32,64 |
| `--save-baseline` | off | Persist results for regression comparison |
| `--check-regression` | off | Compare against saved baseline |
| `--tolerance` | `0.10` | Max allowed degradation (10%) |
| `--dry-run` | off | Show estimated cost, execute nothing |
| `--max-cost-usd` | none | Abort if estimated cost exceeds limit |
| `--confirm-cost` | off | Auto-confirm when cost > $1.00 |
| `--external-config` | none | Path to external framework config JSON |
| `--external-timeout` | `300` | Timeout (seconds) for external runners |
| `--suite` | none | Task quality suite: `tau2` or `bfcl` |
| `--experimental` | off | Enable experimental workloads |

## Built-in Workloads

| Workload | Description | Expected Turns | Tools |
|----------|-------------|---------------|-------|
| `simple_tool_call` | Single tool invocation (weather lookup) | 2 | 1 |
| `multi_step_reasoning` | Sequential tool chain (search → details → shipping) | 4 | 3 |
| `parallel_tool_invocation` | Concurrent tool calls (stock price + news + rating) | 2 | 3 |
| `multi_agent_delegation`* | Coordinator delegates to specialist agents | 5 | 2 |

*Requires `--experimental` flag.

## External Benchmark Protocol (EBP)

Competitor frameworks are benchmarked via subprocess. Each harness receives:
- `BENCH_START_EPOCH_NS` environment variable (monotonic nanoseconds at spawn)
- Workload JSON file path as the last CLI argument

And must output exactly one JSON object on stdout:

```json
{
  "framework": "langgraph",
  "cold_start_us": 45000,
  "first_llm_call_epoch_ns": 1705312800000045000,
  "loop_overhead": {
    "min_us": 120,
    "max_us": 890,
    "mean_us": 340,
    "median_us": 310,
    "p95_us": 780,
    "p99_us": 870,
    "count": 10
  },
  "throughput_agents_per_sec": 12.5,
  "peak_rss_bytes": 52428800,
  "token_overhead": {
    "total_tokens": 1200,
    "user_content_tokens": 950,
    "overhead_tokens": 250
  }
}
```

### Writing a Harness

See `harnesses/` for reference implementations:
- `bench_gemini_sdk.py` — Raw Google Gemini Python SDK (no framework)
- `bench_langgraph.py` — LangGraph ReAct agent

Configure them in `harnesses/external-frameworks.json`:

```json
{
  "frameworks": [
    {
      "name": "my-framework",
      "command": "python3",
      "args": ["/path/to/my_harness.py"],
      "env": [],
      "workingDir": null
    }
  ]
}
```

## Architecture

```
adk-bench/
├── src/
│   ├── lib.rs                  # Public exports
│   ├── config.rs               # BenchConfig, CLI flag mapping
│   ├── runner.rs               # BenchRunner orchestrator
│   ├── workload.rs             # Workload schema, built-in workloads
│   ├── metrics.rs              # DurationStats, BenchmarkResult, MetricCollector
│   ├── memory.rs               # Platform-specific RSS sampling
│   ├── instrumented_llm.rs     # InstrumentedLlm wrapper (temp=0, timing capture)
│   ├── external.rs             # ExternalRunner, EBP protocol
│   ├── formatter.rs            # JSON/table/markdown output
│   ├── error.rs                # BenchError enum
│   └── adapters/
│       ├── mod.rs              # TaskQualityAdapter trait
│       ├── tau2.rs             # τ²-bench adapter (feature: tau2)
│       └── bfcl.rs             # BFCL adapter (feature: bfcl)
├── harnesses/
│   ├── bench_gemini_sdk.py     # Gemini Python SDK EBP harness
│   ├── bench_langgraph.py      # LangGraph EBP harness
│   ├── external-frameworks.json
│   └── simple_tool_call.json   # Workload file for external harnesses
└── tests/
```

## Feature Flags

| Feature | Description |
|---------|-------------|
| `tau2` | τ²-bench task quality adapter |
| `bfcl` | Berkeley Function Calling Leaderboard adapter |

## Design Principles

- **Real LLM calls** — No mocks. Deterministic config (temperature=0, top_p=1.0, seed=42) for reproducibility.
- **Overhead isolation** — `InstrumentedLlm` captures per-call timing; framework overhead = total_turn - llm_round_trip.
- **Apples-to-apples comparison** — All frameworks get the same workload, same model, same BENCH_START_EPOCH_NS clock source.
- **Cost awareness** — `--dry-run`, `--max-cost-usd`, `--confirm-cost` prevent surprise bills.
- **CI-ready** — `--check-regression` exits with code 2 on regressions, integrates with any CI system.
- **Platform-specific memory** — Linux `/proc/self/statm` (authoritative), macOS `mach_task_basic_info` (informational).

## Environment

Requires `GOOGLE_API_KEY` for Gemini models. Set it before running:

```bash
export GOOGLE_API_KEY="your-api-key"
```

For external Python harnesses, ensure dependencies are installed:

```bash
pip install google-generativeai langgraph langchain-google-genai langchain-core
```

## Measurement Notes

- **Cold start** is measured from process spawn (or `BENCH_START_EPOCH_NS`) to the first `generate_content` call timestamp.
- **Agent loop overhead** is computed per-turn by subtracting the LLM round-trip from total turn wall-clock time.
- **CV > 20% warning** is emitted when overhead measurements are unstable — increase `--runs` or reduce system load.
- **Linux is authoritative** for published cross-framework memory comparisons due to consistent `/proc/self/statm` RSS reporting.
- Results measured on Apple M-series, macOS, June 2026. Your numbers will vary by hardware and network.

## License

Apache 2.0
