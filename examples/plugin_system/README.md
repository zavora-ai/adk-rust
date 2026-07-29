# Enhanced Plugin System Demo

Demonstrates the ADK-Rust Enhanced Plugin System (Sprint 1) with three custom plugins that intercept tool calls and model calls in a real agent workflow.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    LLM Agent (Gemini 2.5 Flash)              │
├─────────────────────────────────────────────────────────────┤
│                    Plugin Pipeline                            │
│                                                              │
│  ┌──────────────┐  ┌──────────────────┐  ┌──────────────┐  │
│  │ CachingPlugin│→ │SanitizationPlugin│→ │LoggingPlugin  │  │
│  │ (priority 30)│  │  (priority 50)   │  │(priority 100) │  │
│  └──────────────┘  └──────────────────┘  └──────────────┘  │
│                                                              │
├─────────────────────────────────────────────────────────────┤
│                    Tools: get_weather                         │
└─────────────────────────────────────────────────────────────┘
```

## Plugins

### 1. CachingPlugin (priority 30)
- Runs first in the pipeline
- Checks if the same tool was called with the same arguments before
- On cache HIT: returns `BeforeToolCallResult::ShortCircuit(cached_result)` — skips tool execution entirely
- On cache MISS: passes through, then caches the result in `after_tool_call`
- Uses `PluginContext` to track hit/miss statistics

### 2. SanitizationPlugin (priority 50)
- Runs second in the pipeline
- Injects `safe_mode: true` into all tool arguments before execution
- Adds `sanitized: true` to all tool results after execution
- Demonstrates argument modification in the before-hook

### 3. LoggingPlugin (priority 100)
- Runs last in the pipeline (observes final state)
- Logs all tool calls and model calls with details
- Tracks call counts in `PluginContext` shared state
- Demonstrates both tool and model hook interception

## Prerequisites

- Rust 1.95+
- `GOOGLE_API_KEY` environment variable set

## Setup

```bash
cd examples/plugin_system
cp .env.example .env
# Edit .env and add your Google API key
```

## Run

```bash
cargo run
```

## Expected Output

The example runs three queries:

1. **"What's the weather in London?"** — Cache MISS, tool executes, result cached
2. **"What's the weather in London?"** — Cache HIT, tool execution skipped
3. **"What's the weather in Tokyo?"** — Cache MISS for new city, tool executes

You'll see the plugin pipeline in action with clear output showing:
- Which plugins intercept each call
- Cache hit/miss decisions
- Argument sanitization
- Timing information

## Key Concepts

- **Priority ordering**: Lower values execute first (security → cache → logging)
- **BeforeToolCallResult::ShortCircuit**: Skip tool execution with a synthetic result
- **BeforeToolCallResult::Continue**: Pass (modified) args to next plugin
- **PluginContext**: Type-safe shared state across all plugin hooks
- **EnhancedPlugin trait**: Implement only the hooks you need
