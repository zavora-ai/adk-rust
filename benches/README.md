# ADK-Rust Benchmarks

This directory contains Criterion-based benchmarks for measuring ADK-Rust performance.

## Benchmark Suites

### 1. Agent Benchmarks (`agent_benchmarks.rs`)
- **Simple Agent Execution**: Single-turn conversation with Gemini API
- **Multi-Turn Execution**: 3-turn conversation simulating interactive session

### 2. Streaming Benchmarks (`streaming_benchmarks.rs`)
- **Time-to-First-Token (TTFT)**: Measures latency until first streaming chunk
- **Streaming Throughput**: Total tokens/second during full response streaming

### 3. Template Benchmarks (`template_benchmarks.rs`)
- **Simple Substitution**: Single `{placeholder}` replacement
- **Complex Substitution**: Multiple placeholders with prefixes (`user:`, `app:`)
- **Many Placeholders**: 10+ placeholder replacements in one template

## Running Benchmarks

### Prerequisites
```bash
# Set Gemini API key in .env
echo "GEMINI_API_KEY=your_key_here" > .env
```

### Run All Benchmarks
```bash
cargo bench -p adk-benchmarks
```

### Run Specific Suite
```bash
# Template parsing only (no API calls, fast)
cargo bench -p adk-benchmarks --bench template_benchmarks

# Agent execution (requires API key, slower)
cargo bench -p adk-benchmarks --bench agent_benchmarks

# Streaming metrics (requires API key, slower)
cargo bench -p adk-benchmarks --bench streaming_benchmarks
```

### Save Baseline
```bash
cargo bench -p adk-benchmarks -- --save-baseline initial
```

### Compare Against Baseline
```bash
cargo bench -p adk-benchmarks -- --baseline initial
```

## Baseline Metrics

> **Note**: Actual performance depends on network latency to Gemini API, API model, and hardware.
> Template benchmarks are CPU-bound and more reproducible.

### Template Parsing (Local, CPU-bound)
| Benchmark | Time (est.) | Notes |
|-----------|-------------|-------|
| Simple substitution | ~1-5 µs | Single placeholder |
| Complex substitution | ~5-20 µs | 6 placeholders with prefixes |
| Many placeholders | ~10-30 µs | 10 placeholders |

### Agent Execution (API-bound)
| Benchmark | Time (est.) | Notes |
|-----------|-------------|-------|
| Simple execution | ~500-2000 ms | Single API call + processing |
| Multi-turn (3 turns) | ~1500-6000 ms | 3 sequential API calls |

### Streaming Performance (API-bound)
| Metric | Value (est.) | Notes |
|--------|--------------|-------|
| Time-to-First-Token | ~200-800 ms | Network + model startup |
| Throughput | ~20-100 tokens/sec | Depends on response length |

## Optimization Opportunities

### 1. **Template Parsing - Regex Compilation**
**Location**: `adk-core/src/instruction_template.rs:10`
**Impact**: Medium
**Description**: The placeholder regex is compiled once using `OnceLock`, which is good. However, the regex pattern `\\{+[^{}]*\\}+` is overly permissive and could be optimized for the specific placeholder syntax.

**Recommendation**:
- Use a more specific pattern like `\\{[a-zA-Z_][a-zA-Z0-9_:]*\\??\\}` to match only valid identifiers
- This reduces backtracking and improves performance on malformed input

### 2. **String Allocations in Template Injection**
**Location**: `adk-core/src/instruction_template.rs:131-151`
**Impact**: Low-Medium
**Description**: The `inject_session_state` function creates a new `String` with `String::with_capacity(template.len())`. This capacity might be too small if placeholders expand to large values.

**Recommendation**:
- Profile actual expansion ratios
- Pre-allocate with `template.len() * 1.2` to reduce reallocations
- Consider using `Cow<str>` if templates often have no placeholders

### 3. **Async Overhead in Simple Calls**
**Location**: Various
**Impact**: Low
**Description**: Some simple operations are `async` even when they don't need to be (e.g., state lookups).

**Recommendation**:
- Profile to identify hot paths with unnecessary async overhead
- Consider sync variants for state access where appropriate

### 4. **Streaming Buffer Sizes**
**Location**: Model implementation
**Impact**: Medium (for large responses)
**Description**: Default buffer sizes for streaming might not be optimal.

**Recommendation**:
- Benchmark different chunk sizes
- Tune for latency vs throughput tradeoff

## Future Work

- **Comparison with adk-go**: Once both implementations are feature-complete, add comparative benchmarks
- **Memory profiling**: Use `valgrind`/`heaptrack` to identify allocation hotspots
- **Flamegraph analysis**: Generate flamegraphs to visualize CPU time distribution
- **Real-world workload simulation**: Benchmark against actual agent workflows from production
