# Retry & Reflect Demo

Demonstrates the ADK-Rust Retry & Reflect plugin (Sprint 2) handling tool failures gracefully with exponential backoff and structured reflection prompts.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    LLM Agent (Gemini 2.5 Flash)              │
├─────────────────────────────────────────────────────────────┤
│                    RetryReflectPlugin                         │
│                                                              │
│  Tool fails → Detect error → Compute backoff → Sleep        │
│            → Render reflection prompt → Return to agent      │
│            → Agent retries with guidance                     │
│                                                              │
├─────────────────────────────────────────────────────────────┤
│                    FlakySearchTool                            │
│                                                              │
│  Attempt 1: ❌ 503 Service Unavailable                      │
│  Attempt 2: ❌ 429 Rate Limited                             │
│  Attempt 3: ✅ Success! Returns search results              │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

## How It Works

### The Retry-Reflect Cycle

1. **Agent calls tool** → `search_web("Rust programming")`
2. **Tool fails** → Returns `{"error": "503 Service Unavailable"}`
3. **Plugin detects error** → `after_tool_call` hook sees error pattern
4. **Plugin computes backoff** → Exponential: 100ms × 2^(attempt-1)
5. **Plugin sleeps** → Waits for backoff duration
6. **Plugin renders reflection** → Structured prompt with error details + guidance
7. **Agent receives reflection** → Understands the failure and retries
8. **Repeat until success** → Or max retries (3) exhausted

### Exponential Backoff

| Attempt | Backoff Delay | Cumulative Wait |
|---------|---------------|-----------------|
| 1       | 100ms         | 100ms           |
| 2       | 200ms         | 300ms           |
| 3       | 400ms         | 700ms           |
| (max)   | capped at 5s  | —               |

### Reflection Prompt

When a tool fails, the plugin injects a structured reflection like:

```
Tool 'search_web' failed on attempt 1/3.
Error: Connection timeout: upstream search service unavailable (503)
Arguments: {"query": "Rust programming"}

Please reflect on this failure and try again with corrected or alternative arguments.
```

This gives the agent context to self-correct on the next turn.

## Prerequisites

- Rust 1.95+
- `GOOGLE_API_KEY` environment variable set

## Setup

```bash
cd examples/retry_reflect
cp .env.example .env
# Edit .env and add your Google API key
```

## Run

```bash
cargo run
```

## Expected Output

```
╔══════════════════════════════════════════════════════════════╗
║  Retry & Reflect Demo — ADK-Rust Sprint 2                   ║
╚══════════════════════════════════════════════════════════════╝

✓ Model: gemini-2.5-flash

── Configuring RetryReflectPlugin ────────────────────────────────
  • Max retries: 3
  • Backoff: Exponential (base=100ms, max=5s)
  • Priority: 200

    [FlakySearchTool] Attempt #1 for query: "Rust programming"
    [FlakySearchTool] ❌ FAILURE (attempt #1): Connection timeout...
  ⚡ [RetryReflect] retry_reflect.retry: attempt=1, backoff=100ms

    [FlakySearchTool] Attempt #2 for query: "Rust programming"
    [FlakySearchTool] ❌ FAILURE (attempt #2): Rate limited...
  ⚡ [RetryReflect] retry_reflect.retry: attempt=2, backoff=200ms

    [FlakySearchTool] Attempt #3 for query: "Rust programming"
    [FlakySearchTool] ✅ SUCCESS (attempt #3)

  🤖 Agent response: Rust is a systems programming language...
```

## Configuration Options

The `RetryReflectPluginBuilder` supports:

| Option | Description | Default |
|--------|-------------|---------|
| `max_retries(n)` | Max retry attempts per tool | 3 |
| `backoff_exponential(base)` | Exponential backoff with base delay | None |
| `backoff_fixed(delay)` | Fixed delay between retries | None |
| `max_backoff(ceiling)` | Maximum backoff duration | 30s |
| `per_tool_limit(name, n)` | Override max retries for specific tool | — |
| `global_limit(n)` | Total retries across all tools | — |
| `allowlist(tools)` | Only retry these tools | All |
| `denylist(tools)` | Never retry these tools | None |
| `priority(n)` | Plugin execution priority | 200 |
| `enable_global_tracking(n)` | Circuit-breaker threshold | — |

## Key Concepts

- **Error Detection**: Plugin checks if tool result contains `{"error": ...}` pattern
- **Backoff Strategy**: Prevents overwhelming failing services
- **Reflection Prompts**: Guide the agent to self-correct
- **Circuit Breaker**: Global tracking prevents runaway retries
- **Tracing Events**: `retry_reflect.retry` and `retry_reflect.exhausted` for observability
