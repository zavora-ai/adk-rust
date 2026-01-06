# Graph Agent Test Examples

This project contains working examples that demonstrate the key concepts from the graph-agents.md documentation.

## Examples

| Example | Description | Key Concepts |
|---------|-------------|--------------|
| `parallel_processing` | Translation and summarization in parallel | Parallel execution, AgentNode, state flow |
| `conditional_routing` | Sentiment-based routing to different handlers | Conditional edges, LLM classification |
| `react_pattern` | Iterative reasoning with tools | Cyclic graphs, tool usage, iteration limits |
| `supervisor_routing` | Route tasks to specialist agents | Multi-agent coordination, dynamic routing |
| `human_in_loop` | Risk-based approval workflow | Dynamic interrupts, checkpointing |
| `checkpointing` | State persistence and time travel | SQLite checkpointer, history navigation |

## Setup

1. Set your API key:
```bash
echo 'GOOGLE_API_KEY=your-api-key' > .env
```

2. Run examples:
```bash
# Parallel processing example
cargo run --bin parallel_processing

# Conditional routing with sentiment analysis
cargo run --bin conditional_routing

# ReAct pattern with tools
cargo run --bin react_pattern

# Supervisor routing to specialists
cargo run --bin supervisor_routing

# Human-in-the-loop approval
cargo run --bin human_in_loop

# Checkpointing and state persistence
cargo run --bin checkpointing
```

## Example Outputs

### Parallel Processing
```
=== Processing Complete ===

French Translation:
L'IA transforme notre façon de travailler et de vivre.

Summary:
AI is revolutionizing work and daily life through technological transformation.
```

### Conditional Routing
```
Input: "Your product is amazing! I love it!"
Sentiment: positive
Response: Thank you so much for the wonderful feedback! We're thrilled you love our product. Would you consider leaving a review to help others?
```

### ReAct Pattern
```
Question: "What's the weather in Paris and what's 15 + 25?"
Final answer: The weather in Paris is 72°F and sunny with 45% humidity. And 15 + 25 equals 40.
Iterations: 2
```

## Key Learning Points

1. **AgentNode Pattern**: Wrap LLM agents with input/output mappers
2. **State Flow**: How data moves through the graph between nodes
3. **Parallel Execution**: Multiple nodes running simultaneously
4. **Conditional Logic**: Dynamic routing based on state values
5. **Cyclic Workflows**: Iterative reasoning with safety limits (max 3 iterations)
6. **Human Oversight**: Interrupt execution for approval
7. **State Persistence**: Checkpointing for fault tolerance

## Safety Features

All examples include safety limits to prevent infinite loops:
- **ReAct Pattern**: Maximum 3 iterations
- **Supervisor Routing**: Maximum 3 iterations with completion detection
- **Human-in-the-Loop**: Recursion limit of 3
- **Conditional Routing**: Single-pass execution
- **Parallel Processing**: Linear execution flow
- **Checkpointing**: Linear workflow with step tracking
