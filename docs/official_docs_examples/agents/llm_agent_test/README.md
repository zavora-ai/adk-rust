# LlmAgent Test

Test examples for the [LlmAgent](../../docs/official_docs/agents/llm-agent.md) documentation.

## Prerequisites

Set your API key:
```bash
echo 'GOOGLE_API_KEY=your-key' > .env
```

## Examples

### Basic Agent
Minimal agent with instruction:
```bash
cargo run --bin basic_agent
```

### Shaped Behavior
Different instruction personalities:
```bash
cargo run --bin shaped_behavior -- formal
cargo run --bin shaped_behavior -- tutor
cargo run --bin shaped_behavior -- storyteller

# Or via environment variable
AGENT_TYPE=tutor cargo run --bin shaped_behavior
```

### Instruction Templating
Template variables from session state:
```bash
cargo run --bin instruction_templating
```

### Multi Tools
Agent with weather and calculator tools:
```bash
cargo run --bin multi_tools
# Try: "What's 15% of 250?" or "Weather in Tokyo"
```

### Structured Output
JSON schema for entity extraction:
```bash
cargo run --bin structured_output
# Try: "John met Sarah in Paris on December 25th"
```

### Complete Example
Full production agent with tools:
```bash
cargo run --bin complete_example
```

## Build All

```bash
cargo build
```
