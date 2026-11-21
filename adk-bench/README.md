# adk-bench

Benchmarking framework for ADK-Rust: measures framework-level runtime overhead against real LLM
APIs and compares ADK-Rust with other agent frameworks through a subprocess protocol.

[![Crates.io](https://img.shields.io/crates/v/adk-bench.svg)](https://crates.io/crates/adk-bench)
[![Documentation](https://docs.rs/adk-bench/badge.svg)](https://docs.rs/adk-bench)
[![License](https://img.shields.io/crates/l/adk-bench.svg)](../LICENSE)

## How it works

```text
┌─────────────────────────────────────────────────────────────────┐
│ cargo adk bench                                                 │
├─────────────────────────────────────────────────────────────────┤
│  BenchRunner                                                    │
│  ├── Warm-up phase (iterations discarded)                       │
│  ├── Measurement phase                                          │
│  │   ├── InstrumentedLlm → real API (temperature 0, seed 42)    │
│  │   ├── Record: request_sent → response_complete               │
│  │   └── Overhead = total turn time − LLM round trip            │
│  ├── Concurrency sweep (1, 2, 4, 8, 16, 32, 64)                 │
│  └── Regression detection (baseline compare)                    │
│                                                                 │
│  ExternalRunner (External Benchmark Protocol)                   │
│  ├── Spawn subprocess with BENCH_START_EPOCH_NS                  │
│  ├── Pass the workload JSON path as the last argument           │
│  ├── Parse one EBP JSON object from stdout                      │
│  └── Kill the subprocess when it exceeds the timeout            │
└─────────────────────────────────────────────────────────────────┘
```

### Metrics

| Metric | Source | Description |
|--------|--------|-------------|
| Cold start | `BenchRunner` | Run start (model creation) to the first LLM request; for external frameworks, process spawn to the first LLM call |
| Agent loop overhead | `BenchRunner` | Per-turn framework time: total turn time minus the LLM round trip |
| Concurrent throughput | `BenchRunner` with `--sweep` or `--concurrency` | Agents completed per second at each concurrency level |
| Memory | `BenchRunner` | Peak process RSS during the measurement phase, sampled every `memory_sample_interval_ms`, and the growth per concurrent agent; Linux and macOS only |
| Peak RSS, token overhead | External harnesses (EBP) | Reported by the harness in its JSON output |

## Vision benchmarks

Feature `vision` measures `adk-vision` backends (spec 3.7 §7.17): cold load, p50 and p95 per
image at three input sizes, throughput, peak RSS, accuracy on the vision eval sets (scored by
`adk-eval`'s vision criteria) and cost per 1,000 images, published with the machine, OS, CPU
and runtime versions. `adapters::vision::VisionAdapter` reports a backend's pass rate on an
eval set beside the τ²-bench and BFCL adapters.

| Feature | Backends |
|---------|----------|
| `vision-onnx` (`vision-coreml`) | RF-DETR Nano fp32 and q4f16, EfficientSAM-Ti, SAM 2.1 Tiny q4, SigLIP 2 base |
| `vision-ocr` | PP-OCRv6 tiny and small |
| `vision-ocr-vl` (`vision-ocr-vl-metal`) | PaddleOCR-VL-1.6 on the CPU (and Metal) |
| `vision-cloud` | Claude, Nemotron, Gemma 4 and Muse describers and Meta `sam-3.1`, from recorded responses or one live call each |

```bash
HF_HUB_CACHE=/path/to/models cargo run -p adk-bench --profile ci \
    --features vision-onnx,vision-ocr,vision-cloud --example vision_bench -- \
    run --out vision-bench.md --json vision-bench.json
```

`vision_bench serve <backend>` answers the `cargo adk eval` agent protocol with one local
backend. Results are on [Vision Benchmarks](../docs/official_docs/vision/benchmarks.md).

`memory::current_rss_bytes` and `memory::spawn_memory_sampler` read the RSS from
`/proc/self/statm` on Linux and `mach_task_basic_info` on macOS. `BenchmarkResult::model` is
the model the run called (`BenchConfig::model`).

## Usage

`cargo adk bench` (from `cargo-adk`) drives this crate. It needs `cargo-adk`'s `bench` feature,
off by default:

```bash
cargo install cargo-adk --features bench
```

### Basic run

```bash
# Run all built-in workloads with default settings
cargo adk bench --confirm-cost

# Single workload, minimal cost
cargo adk bench --workload simple_tool_call --runs 2 --warmup 1 --confirm-cost
```

### Cost control

```bash
# See the estimated cost without making API calls
cargo adk bench --dry-run

# Set a hard cost limit
cargo adk bench --max-cost-usd 5.00 --confirm-cost
```

### Framework comparison

```bash
cargo adk bench --workload simple_tool_call --runs 3 \
  --external-config adk-bench/harnesses/external-frameworks.json \
  --format markdown --confirm-cost
```

### Concurrency sweep

```bash
cargo adk bench --workload simple_tool_call --sweep --runs 3 --confirm-cost
```

### Regression detection (CI)

```bash
# Save a baseline
cargo adk bench --save-baseline --confirm-cost

# Check for regressions (exit code 2 when a metric regressed)
cargo adk bench --check-regression --tolerance 0.10 --confirm-cost
```

### Output formats

```bash
cargo adk bench --format json --output results.json --confirm-cost   # all raw metrics
cargo adk bench --format markdown --confirm-cost                      # comparison table
cargo adk bench --format table --confirm-cost                         # terminal display (default)
```

## CLI reference

| Flag | Default | Description |
|------|---------|-------------|
| `--model` | `gemini-3.8-flash` | LLM model identifier |
| `--runs` | `5` | Measurement iterations per workload |
| `--warmup` | `3` | Warm-up iterations (discarded) |
| `--concurrency` | `1` | Agent concurrency level |
| `--workload` | all built-in | Workload name or file path |
| `--format` | `table` | `table`, `json`, or `markdown` |
| `--output` | stdout | Write results to a file |
| `--sweep` | off | Test concurrency levels 1, 2, 4, 8, 16, 32, 64 |
| `--save-baseline` | off | Persist results for regression comparison |
| `--check-regression` | off | Compare against the saved baseline |
| `--tolerance` | `0.10` | Maximum allowed degradation (10%) |
| `--dry-run` | off | Show the estimated cost and execute nothing |
| `--max-cost-usd` | none | Abort when the estimated cost exceeds the limit |
| `--confirm-cost` | off | Skip the interactive confirmation when the cost exceeds $1.00 |
| `--external-config` | none | Path to an external framework config JSON |
| `--external-timeout` | `300` | Timeout in seconds for external runners |
| `--experimental` | off | Include experimental workloads |

`BenchConfig::default()`, used when the library is called directly, selects
`gemini-3.5-flash-lite`.

## Built-in workloads

| Workload | Description | Expected turns | Tools |
|----------|-------------|----------------|-------|
| `simple_tool_call` | Single tool invocation (weather lookup) | 2 | 1 |
| `multi_step_reasoning` | Sequential tool chain (search → details → shipping) | 4 | 3 |
| `parallel_tool_invocation` | Concurrent tool calls (stock price, news, rating) | 2 | 3 |
| `multi_agent_delegation` | Coordinator delegates to specialist agents; needs `--experimental` | 5 | 2 |

## Long history (no API key)

`long_history` measures the framework's own cost as a session grows, with a scripted model
that answers instantly, an in-memory session, and no API key. A step is one turn
(`turns`: a user message, a tool call and the answer) or one tool iteration inside a single
autonomous run (`loop`). Work that copies or rescans the whole history shows as a per-step cost
that rises with the step number.

```bash
cargo run -p adk-bench --release --example long_history -- turns 1000
cargo run -p adk-bench --release --example long_history -- turns 1000 --window 50
cargo run -p adk-bench --release --example long_history -- loop 2000 --payload 4096 --json
```

| Flag | Default | Description |
|------|---------|-------------|
| `turns` / `loop` | required | What a step is |
| `<steps>` | required | Number of turns, or tool iterations of the one run |
| `--window` | none | `RunConfig::history_max_events` |
| `--payload` | `200` | Bytes of text each tool result carries |
| `--json` | off | Print the `LongHistoryReport` as JSON |

The report gives the mean, median and p95 process CPU time per step for the first and last
tenth of the run, their ratio (`growth`), the total process CPU time, and the peak RSS.
`tests/long_history_allocations.rs` runs it in every pull request's test suite: it counts the
bytes each turn allocates, and fails when a `history_max_events` window stops bounding a turn's
work or when a turn copies the history more often than it did.

## Library use

```toml
[dependencies]
adk-bench = "3.0.0"
```

```rust
use adk_bench::{BenchConfig, BenchRunner};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = BenchConfig {
        workload: Some("simple_tool_call".to_string()),
        runs: 1,
        warmup: 0,
        ..BenchConfig::default()
    };
    let results = BenchRunner::new(config).run().await?;
    for result in &results {
        println!("{}: overhead mean {} µs", result.workload_name, result.agent_loop_overhead.mean_us);
    }
    Ok(())
}
```

### Features

| Feature | Default | Adds |
|---------|---------|------|
| `tau2` | no | `adapters::tau2::Tau2Adapter`, a τ²-bench task-quality adapter |
| `bfcl` | no | `adapters::bfcl::BfclAdapter`, a Berkeley Function Calling Leaderboard adapter |

## External Benchmark Protocol (EBP)

Other frameworks are benchmarked as subprocesses. Each harness receives:

- the `BENCH_START_EPOCH_NS` environment variable (nanosecond timestamp at spawn)
- the workload JSON file path as the last command-line argument

and prints exactly one JSON object on stdout:

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

A harness that runs past `--external-timeout` is killed and reported as
`BenchError::ExternalTimeout`.

### Writing a harness

`harnesses/` has reference implementations:

- `bench_gemini_sdk.py` — the Google Gemini Python SDK without a framework
- `bench_langgraph.py` — a LangGraph ReAct agent

Configure harnesses in a JSON file, as `harnesses/external-frameworks.json` does. A relative
script path or `workingDir` resolves against the directory `cargo adk bench` runs in; the
bundled file expects the repository root:

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

## Design principles

- **Real LLM calls** — no mocks; `InstrumentedLlm` sends temperature 0, `top_p` 1.0, and seed 42.
- **Overhead isolation** — `InstrumentedLlm` records per-call timing; framework overhead is the
  total turn time minus the LLM round trip.
- **Same inputs** — every framework gets the same workload, model, and `BENCH_START_EPOCH_NS`
  clock source.
- **Cost awareness** — `--dry-run`, `--max-cost-usd`, and `--confirm-cost`.
- **CI** — `--check-regression` exits with code 2 on a regression.

## Environment

Gemini models need `GOOGLE_API_KEY`:

```bash
export GOOGLE_API_KEY="your-api-key"
```

The Python harnesses need their dependencies:

```bash
pip install google-generativeai langgraph langchain-google-genai langchain-core
```

## Measurement notes

- **CV warning** — a coefficient of variation above 20% is reported as unstable; increase
  `--runs` or reduce system load.
- **Memory** — Linux `/proc/self/statm` is the reference for published cross-framework memory
  comparisons; macOS figures are informational.
- Results vary by hardware, network, and model; compare runs only within one model generation.

## License

Apache-2.0
